//! Unit tests of `sched::uv` in threads mode (`mt::uv`). The loop lock's
//! tests use a lock of their own and real threads (Miri runs them); the
//! timers' and signals' use the process's loop thread, `poll(2)`, an eventfd
//! and signal handlers (not under Miri). Each test has a task manager of
//! its own for its promises (`bind_local`): the test's promise resolves
//! through it, from whatever thread the loop resolves it on.

use super::super::task::{self, bind_local, configure, Shared};
use super::super::tests::serial;
use super::super::{Glue, TaskId};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex as StdMutex;
use std::thread::ThreadId;

struct TestGlue;
impl Glue for TestGlue {}

fn start_test() -> Arc<Shared> {
    let sh = bind_local();
    configure(&sh, Arc::new(TestGlue), 2, 256 << 10);
    sh
}

/// What runs on the resolving thread right after a resolution (a stand-in
/// for a `sync` dependent).
type Hook = Box<dyn FnOnce() + Send>;

/// A translator's promise: the test's task manager, the promise's task, a
/// slot with the value and the thread that resolved it.
#[derive(Clone)]
struct P(Arc<Inner>);

struct Inner {
    sh: Arc<Shared>,
    id: TaskId,
    slot: StdMutex<Option<(i64, ThreadId)>>,
    hook: StdMutex<Option<Hook>>,
}

impl P {
    fn new(sh: &Arc<Shared>) -> P {
        P(Arc::new(Inner {
            sh: sh.clone(),
            id: task::promise_new(sh).expect("the task manager runs"),
            slot: StdMutex::new(None),
            hook: StdMutex::new(None),
        }))
    }
    fn maker(sh: &Arc<Shared>) -> impl Fn() -> P + '_ {
        move || P::new(sh)
    }
    fn on_resolve(&self, f: impl FnOnce() + Send + 'static) {
        *self.0.hook.lock().unwrap() = Some(Box::new(f));
    }
    /// `IO.wait p.result?`: the value, once resolved.
    fn get(&self) -> (i64, ThreadId) {
        task::wait(&self.0.sh, self.0.id);
        self.0.slot.lock().unwrap().expect("resolved")
    }
    fn same(&self, o: &P) -> bool {
        Arc::ptr_eq(&self.0, &o.0)
    }
}

impl LoopPromise for P {
    fn is_resolved(&self) -> bool {
        self.0.slot.lock().unwrap().is_some()
    }
    fn resolve(&self, v: i64) {
        let me = self.clone();
        let stored = task::resolve(&self.0.sh, self.0.id, move || {
            *me.0.slot.lock().unwrap() = Some((v, std::thread::current().id()));
        });
        let hook = self.0.hook.lock().unwrap().take();
        if let (true, Some(h)) = (stored, hook) {
            h();
        }
    }
}

// ---------------------------------------------------------------------------
// The loop lock

/// Native's `event_loop_lock`: recursive on one thread; another thread
/// counts itself a waiter, interrupts the holder and waits; the loop thread
/// takes the lock again only once no waiter is left, so the waiter goes
/// first (`event_loop.cpp` 66-91).
#[test]
fn the_loop_lock_is_recursive_and_a_waiter_goes_before_the_loop() {
    let lk = LoopLock::new();
    let order: StdMutex<Vec<&'static str>> = StdMutex::new(Vec::new());
    let interrupted = AtomicBool::new(false);
    // this thread plays the loop thread: it holds the lock, twice
    lk.acquire_as_loop();
    lk.acquire(|| panic!("no interrupt on a recursive lock"));
    assert!(lk.held_here());
    lk.release();
    assert!(lk.held_here());
    std::thread::scope(|sc| {
        sc.spawn(|| {
            lk.acquire(|| interrupted.store(true, Ordering::SeqCst));
            order.lock().unwrap().push("requester");
            lk.release();
        });
        // the requester interrupts, then waits for the loop's release
        while !interrupted.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        order.lock().unwrap().push("loop iteration ends");
        lk.release();
        // the loop thread's next iteration comes after the waiter's turn
        lk.acquire_as_loop();
        order.lock().unwrap().push("loop again");
        lk.release();
    });
    assert_eq!(
        *order.lock().unwrap(),
        ["loop iteration ends", "requester", "loop again"]
    );
    assert!(!lk.held_here());
}

// ---------------------------------------------------------------------------
// Timers

#[test]
#[cfg_attr(miri, ignore)]
fn a_one_shot_timer_resolves_once_on_the_loop_thread() {
    let _s = serial();
    let sh = start_test();
    let t: Timer<P> = Timer::new(20, false);
    let p = t.next(P::maker(&sh));
    assert!(t.next(P::maker(&sh)).same(&p));
    let (v, by) = p.get();
    assert_eq!(v, 0);
    assert_ne!(
        by,
        std::thread::current().id(),
        "resolved by the loop thread"
    );
    assert!(t.next(P::maker(&sh)).same(&p));
    t.stop();
    let q = t.next(P::maker(&sh));
    assert!(!q.same(&p) && !q.is_resolved());
    // a second timer: the same loop thread
    let u: Timer<P> = Timer::new(1, false);
    assert_eq!(u.next(P::maker(&sh)).get().1, by);
    task::finish(&sh);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_repeating_timer_ticks_at_once_then_every_period() {
    let _s = serial();
    // As the single-thread test: a check that a tick has not happened yet
    // holds only while less than a period has passed (review RS2-11).
    const PERIOD: u64 = 100;
    let before = |t0: Instant, periods: u64, what: &str| {
        let on_time = t0.elapsed() < Duration::from_millis(PERIOD * periods);
        if !on_time {
            eprintln!("note: the host stalled past tick {periods}: {what} is not checked");
        }
        on_time
    };
    let sh = start_test();
    let t: Timer<P> = Timer::new(PERIOD, true);
    let t0 = Instant::now();
    let p = t.next(P::maker(&sh));
    assert_eq!(p.get().0, 0);
    let a = t.next(P::maker(&sh));
    assert!(!a.same(&p));
    let again = t.next(P::maker(&sh));
    if before(t0, 1, "the same promise until it resolves") {
        assert!(again.same(&a), "the same promise until it resolves");
    }
    assert_eq!(a.get().0, 0);
    assert!(t0.elapsed() >= Duration::from_millis(PERIOD));
    // cancel drops the promise; the timer ticks on
    let b = t.next(P::maker(&sh));
    t.cancel();
    let b_dropped_unresolved = before(t0, 2, "the cancel before the second tick");
    let c = t.next(P::maker(&sh));
    assert!(!c.same(&b));
    assert_eq!(c.get().0, 0);
    if b_dropped_unresolved {
        assert!(!b.is_resolved());
    }
    t.stop();
    task::finish(&sh);
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_cancelled_one_shot_timer_starts_anew() {
    let _s = serial();
    const TIMEOUT: u64 = 100;
    let sh = start_test();
    let t: Timer<P> = Timer::new(TIMEOUT, false);
    let t0 = Instant::now();
    let p = t.next(P::maker(&sh));
    t.cancel();
    let on_time = t0.elapsed() < Duration::from_millis(TIMEOUT);
    if !on_time {
        eprintln!("note: the host stalled past the timeout: the cancel before it is not checked");
    }
    let q = t.next(P::maker(&sh));
    let fired = q.same(&p);
    if on_time {
        assert!(!fired, "a new promise after the cancel");
    }
    assert_eq!(q.get().0, 0);
    assert_eq!(p.is_resolved(), fired);
    task::finish(&sh);
}

/// An extern waits only for the loop's iteration, not for the loop's next
/// timer: the loop thread waits in `poll(2)` for a timer a minute away, and
/// `Timer.mk` from this thread interrupts it (`uv_async_send`) and returns.
#[test]
#[cfg_attr(miri, ignore)]
fn an_extern_interrupts_the_loop_thread() {
    let _s = serial();
    let sh = start_test();
    let far: Timer<P> = Timer::new(60_000, false);
    let p = far.next(P::maker(&sh));
    std::thread::sleep(Duration::from_millis(20));
    let t0 = Instant::now();
    for _ in 0..10 {
        let _ = Timer::<P>::new(1, false);
    }
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "ten externs waited {:?}",
        t0.elapsed()
    );
    assert!(!p.is_resolved());
    far.stop();
    task::finish(&sh);
}

/// A resolution on the loop thread runs its `sync` dependents there, with
/// the loop lock held: an extern they call goes on at once (the lock is
/// recursive), and a one-shot timer is finished there already (LB-20 not
/// copied: its `stop` releases nothing twice, and `next` then gives a new
/// promise that never resolves).
#[test]
#[cfg_attr(miri, ignore)]
fn externs_in_a_resolution_on_the_loop_thread_go_on_at_once() {
    let _s = serial();
    let sh = start_test();
    let t: Timer<P> = Timer::new(10, false);
    let p = t.next(P::maker(&sh));
    let seen: Arc<StdMutex<Option<(bool, bool)>>> = Arc::default();
    {
        let (t, sh, seen) = (t.clone(), sh.clone(), seen.clone());
        p.on_resolve(move || {
            // on the loop thread, which holds the loop lock
            let inner: Timer<P> = Timer::new(5, false);
            let q = inner.next(P::maker(&sh));
            t.stop();
            let r = t.next(P::maker(&sh));
            inner.stop();
            *seen.lock().unwrap() = Some((!q.is_resolved(), !r.is_resolved()));
        });
    }
    let (_, by) = p.get();
    // the hook ran right after the store, on the resolving thread
    let deadline = Instant::now() + Duration::from_secs(30);
    while seen.lock().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(*seen.lock().unwrap(), Some((true, true)));
    assert_ne!(by, std::thread::current().id());
    // the timer stopped in the dependent: a new promise, unresolved
    let s = t.next(P::maker(&sh));
    assert!(!s.same(&p) && !s.is_resolved());
    task::finish(&sh);
}

// ---------------------------------------------------------------------------
// Signals

#[test]
#[cfg_attr(miri, ignore)]
fn an_unknown_signal_is_refused_at_next() {
    let _s = serial();
    let sh = start_test();
    let s: Signal<P> = Signal::new(99, false);
    let e = s.next(P::maker(&sh)).err().expect("refused");
    assert_eq!(e, -22);
    // left running with that promise, as natively
    let p = s.next(P::maker(&sh)).unwrap();
    assert!(s.next(P::maker(&sh)).unwrap().same(&p));
    assert!(!p.is_resolved());
    s.stop().unwrap();
    task::finish(&sh);
}

/// The child process of `one_signal_reaches_the_watchers_of_every_thread`.
const SIGNAL_CHILD: &str = "LEAN_RUNTIME_TEST_MT_UV_SIGNAL_CHILD";

/// One delivery for the process (docs/threads.md, 3.2): a SIGUSR2 reaches
/// a repeating watcher started on this thread and a one-shot one started in
/// a task on a worker, each resolved on the loop thread, repeating first.
/// In a child process (this test binary again): the signal's handlers stay
/// installed for the life of the process.
#[test]
#[cfg_attr(miri, ignore)]
fn one_signal_reaches_the_watchers_of_every_thread() {
    if std::env::var_os(SIGNAL_CHILD).is_some() {
        signal_child();
        return;
    }
    let _s = serial();
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "sched::mt::uv::tests::one_signal_reaches_the_watchers_of_every_thread",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SIGNAL_CHILD, "1")
        .output()
        .expect("the child runs");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn signal_child() {
    let sh = start_test();
    let rep: Signal<P> = Signal::new(12, true);
    let a = rep.next(P::maker(&sh)).unwrap();
    // a one-shot watcher started in a task, on a worker thread
    type Started = Option<(Signal<P>, P, ThreadId)>;
    let slot: Arc<StdMutex<Started>> = Arc::default();
    let job_slot = slot.clone();
    let job_sh = sh.clone();
    let id = task::spawn(
        &sh,
        Box::new(move || {
            let one: Signal<P> = Signal::new(12, false);
            let b = one.next(P::maker(&job_sh)).unwrap();
            *job_slot.lock().unwrap() = Some((one, b, std::thread::current().id()));
            super::super::Outcome::Done
        }),
        0,
        true,
    )
    .ok()
    .expect("a task");
    task::wait(&sh, id);
    let (one, b, worker) = slot.lock().unwrap().take().expect("the task ran");
    assert_ne!(worker, std::thread::current().id());
    let order: Arc<StdMutex<Vec<&'static str>>> = Arc::default();
    for (p, name) in [(&a, "repeating"), (&b, "one-shot")] {
        let order = order.clone();
        p.on_resolve(move || order.lock().unwrap().push(name));
    }
    rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::USR2)
        .unwrap();
    let (va, by_a) = a.get();
    let (vb, by_b) = b.get();
    // each hook runs right after its resolution, on the loop thread
    let deadline = Instant::now() + Duration::from_secs(30);
    while order.lock().unwrap().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!((va, vb), (12, 12));
    assert_eq!(by_a, by_b, "both on the loop thread");
    assert_ne!(by_a, std::thread::current().id());
    assert_ne!(by_a, worker);
    assert_eq!(*order.lock().unwrap(), ["repeating", "one-shot"]);
    // the one-shot watcher has finished; the repeating one listens on
    assert!(one.next(P::maker(&sh)).unwrap().same(&b));
    let c = rep.next(P::maker(&sh)).unwrap();
    assert!(!c.same(&a) && !c.is_resolved());
    rep.stop().unwrap();
    one.stop().unwrap();
    task::finish(&sh);
}

// ---------------------------------------------------------------------------
// Review RT2 (threads-2): regression tests

/// Runs `test` (this test binary again, `--exact`) in a child process with
/// `env` set, and returns its output; the child's stderr is shown.
fn run_child(test: &str, env: &str) -> std::process::Output {
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(env, "1")
        .output()
        .expect("the child runs");
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    out
}

#[cfg(feature = "io")]
const RT2_04_CHILD: &str = "LEAN_RUNTIME_TEST_RT2_04_CHILD";

/// RT2-04: the loop's eventfd is chosen once, when the loop is made. Here
/// native's startup descriptors are opened after the loop's first use: the
/// loop keeps the eventfd it chose, and an extern interrupts it through that
/// one. Before the fix the loop chose at each iteration and an extern at
/// each interrupt, so the loop polled its own eventfd while the extern wrote
/// native's, and the extern waited for the loop's next timer (8 s). In a
/// child process (the startup descriptors are the process's).
#[cfg(feature = "io")]
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_04_late_startup_descriptors_keep_the_interrupt() {
    if std::env::var_os(RT2_04_CHILD).is_none() {
        let _s = serial();
        let out = run_child(
            "sched::mt::uv::tests::rt2_04_late_startup_descriptors_keep_the_interrupt",
            RT2_04_CHILD,
        );
        assert!(out.status.success());
        return;
    }
    let sh = start_test();
    let far: Timer<P> = Timer::new(8_000, false);
    let _p = far.next(P::maker(&sh));
    std::thread::sleep(Duration::from_millis(200));
    crate::io::startup::open_native_descriptors().expect("descriptors");
    let t0 = Instant::now();
    let _ = Timer::<P>::new(1, false);
    let dt = t0.elapsed();
    eprintln!("RT2-04: one extern after open_native_descriptors waited {dt:?}");
    assert!(dt < Duration::from_secs(4), "lost wake-up: {dt:?}");
}

/// RT2-09: a repeating timer starts its next period from the iteration's
/// time (libuv's `uv_timer_again` adds the repeat to `loop->time`, cached
/// when the iteration began), not from the time its callback runs. Two
/// timers fall due in one iteration (the test holds the loop lock past both
/// deadlines); the first one's resolution computes for 400 ms on the loop
/// thread; the repeating one (period 300 ms) then fires, and its next
/// period, counted from the iteration's time, is already over: its next
/// tick comes at the next iteration. Counted from its callback (before the
/// fix), it came 300 ms later. The promise of that next tick is made by the
/// late tick's own `sync`-like hook, on the loop thread, so it exists
/// before the next iteration.
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_09_a_repeating_timer_restarts_from_the_iterations_time() {
    let _s = serial();
    let sh = start_test();
    let b: Timer<P> = Timer::new(300, true);
    let b0 = b.next(P::maker(&sh));
    b0.get(); // the 0th tick
    let b1 = b.next(P::maker(&sh));
    let late: Arc<StdMutex<Option<(Instant, P)>>> = Arc::default();
    {
        let (b, sh, late) = (b.clone(), sh.clone(), late.clone());
        b1.on_resolve(move || {
            let b2 = b.next(P::maker(&sh));
            *late.lock().unwrap() = Some((Instant::now(), b2));
        });
    }
    let a: Timer<P> = Timer::new(100, false);
    let pa = a.next(P::maker(&sh));
    pa.on_resolve(|| std::thread::sleep(Duration::from_millis(400)));
    // both fall due while the loop waits for its lock
    let lp = the_loop();
    lp.lock.acquire(|| interrupt(lp));
    std::thread::sleep(Duration::from_millis(400));
    lp.lock.release();
    b1.get();
    let deadline = Instant::now() + Duration::from_secs(30);
    while late.lock().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let (t1, b2) = late.lock().unwrap().take().expect("the hook ran");
    b2.get();
    let gap = t1.elapsed();
    eprintln!("RT2-09: the tick after the late one came {gap:?} later");
    b.stop();
    task::finish(&sh);
    assert!(gap < Duration::from_millis(150), "{gap:?}");
}

/// Runs `test` in a child process with `env` set, as `run_child`; kills it
/// after `secs` seconds and fails if it had not ended by then.
fn run_child_within(test: &str, env: &str, secs: u64) {
    let t0 = Instant::now();
    let mut child = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(env, "1")
        .spawn()
        .expect("the child runs");
    loop {
        if let Some(st) = child.try_wait().expect("the child's status") {
            assert!(st.success(), "{test}: {st:?}");
            return;
        }
        if t0.elapsed() > Duration::from_secs(secs) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{test}: the child did not end within {secs} s (a deadlock)");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const RT2_07_CHILD: &str = "LEAN_RUNTIME_TEST_RT2_07_CHILD";

/// RT2-07: `next` makes its promise with no state lock held, so a glue
/// whose promise maker calls an extern on the same handle (on this thread:
/// the loop lock is recursive) goes on. Before the fix the maker ran under
/// the handle's state lock, and that extern waited for it for good. In a
/// child process, killed after 10 s.
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_07_a_promise_maker_that_calls_an_extern_on_the_handle() {
    if std::env::var_os(RT2_07_CHILD).is_none() {
        let _s = serial();
        run_child_within(
            "sched::mt::uv::tests::rt2_07_a_promise_maker_that_calls_an_extern_on_the_handle",
            RT2_07_CHILD,
            10,
        );
        return;
    }
    let sh = start_test();
    let t: Timer<P> = Timer::new(20, false);
    let t2 = t.clone();
    let p = t.next(|| {
        t2.reset();
        P::new(&sh)
    });
    assert_eq!(p.get().0, 0);
    let s: Signal<P> = Signal::new(28, true);
    let s2 = s.clone();
    let q = s
        .next(|| {
            s2.cancel();
            P::new(&sh)
        })
        .unwrap();
    assert!(!q.is_resolved());
    s.stop().unwrap();
    task::finish(&sh);
}

/// RT2-08 (AR-22 in threads mode): `Timer::placeholder` and
/// `Signal::placeholder` take no loop lock, so they never wait for the
/// loop: made while another thread holds the lock, they return at once,
/// where `Timer::new` waits for the lock.
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_08_a_placeholder_never_waits_for_the_loop() {
    let _s = serial();
    let lp = the_loop();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    std::thread::scope(|sc| {
        sc.spawn(move || {
            lp.lock.acquire(|| interrupt(lp));
            held_tx.send(()).unwrap();
            // let go when told, or after 5 s (so a regression fails, not hangs)
            let _ = go_rx.recv_timeout(Duration::from_secs(5));
            lp.lock.release();
        });
        held_rx.recv().unwrap();
        let t0 = Instant::now();
        let _a: Timer<P> = Timer::placeholder();
        let _b: Signal<P> = Signal::placeholder();
        let made = t0.elapsed();
        // t1 is taken before the spawn, so a late thread start cannot
        // shorten the measured wait: `Timer::new` cannot return before `go`,
        // which is sent 100 ms after t1 (review RT2-17)
        let t1 = Instant::now();
        let waiter = sc.spawn(|| {
            let _c: Timer<P> = Timer::new(1, false);
            Instant::now()
        });
        std::thread::sleep(Duration::from_millis(100));
        go_tx.send(()).unwrap();
        let waited = waiter.join().unwrap() - t1;
        assert!(
            made < Duration::from_secs(1),
            "the placeholders waited {made:?}"
        );
        assert!(
            waited >= Duration::from_millis(100),
            "Timer::new waited {waited:?}"
        );
    });
}

/// A glue that logs `thread_start` with the calling thread.
struct StartLog(Arc<StdMutex<Vec<(String, ThreadId)>>>);

impl Glue for StartLog {
    fn thread_start(&self) {
        self.0
            .lock()
            .unwrap()
            .push(("thread_start".into(), std::thread::current().id()));
    }
}

const RT2_05_CHILD: &str = "LEAN_RUNTIME_TEST_RT2_05_CHILD";

/// RT2-05: a loop made before `sched::start` (a Lean `initialize` that
/// makes a `Timer`) has no glue then; its thread calls the glue's
/// `thread_start` once a glue appears, before it runs translator code (a
/// promise's resolution). Before the fix it never called it. In a child
/// process (the process's task manager).
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_05_a_loop_made_before_start_calls_thread_start() {
    if std::env::var_os(RT2_05_CHILD).is_none() {
        let _s = serial();
        let out = run_child(
            "sched::mt::uv::tests::rt2_05_a_loop_made_before_start_calls_thread_start",
            RT2_05_CHILD,
        );
        assert!(out.status.success());
        return;
    }
    let t: Timer<P> = Timer::new(20, false);
    let log: Arc<StdMutex<Vec<(String, ThreadId)>>> = Arc::default();
    super::super::start_with(Arc::new(StartLog(log.clone())), 2, 256 << 10);
    let sh = task::with_shared(|sh| sh.expect("the process's task manager").clone());
    let p = t.next(P::maker(&sh));
    let l2 = log.clone();
    p.on_resolve(move || {
        l2.lock()
            .unwrap()
            .push(("resolved".into(), std::thread::current().id()));
    });
    let (_, by) = p.get();
    let deadline = Instant::now() + Duration::from_secs(30);
    while log.lock().unwrap().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let l = log.lock().unwrap().clone();
    assert_eq!(
        l,
        vec![("thread_start".into(), by), ("resolved".into(), by)],
        "the loop thread's hooks"
    );
    super::super::finish();
}

/// A promise that another thread resolves at a chosen moment: with its
/// second flag set, its next `is_resolved` reads unresolved and the promise
/// is resolved right after that read, as an `IO.Promise.resolve` from another
/// task lands between two reads of `next` (review RT2-13).
#[derive(Clone, Default)]
struct Flip(Arc<(AtomicBool, AtomicBool)>);

impl LoopPromise for Flip {
    fn is_resolved(&self) -> bool {
        if self.0 .1.swap(false, Ordering::SeqCst) {
            return self.0 .0.swap(true, Ordering::SeqCst);
        }
        self.0 .0.load(Ordering::SeqCst)
    }
    fn resolve(&self, _: i64) {
        self.0 .0.store(true, Ordering::SeqCst);
    }
}

/// RT2-13: `next` decides once whether the handle needs a new promise. A
/// repeating timer's promise resolved by the program between two reads made
/// RT2-07's `next` panic (an abort in a task). Natively `next` reads it once
/// under the loop's lock (`timer.cpp` 158-173).
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_13_timer_next_with_a_promise_resolved_between_its_reads() {
    let _s = serial();
    let t: Timer<Flip> = Timer::new(3_600_000, true);
    let p0 = t.next(Flip::default);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !p0.0 .0.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    let p1 = t.next(Flip::default);
    assert!(!Arc::ptr_eq(&p0.0, &p1.0));
    p1.0 .1.store(true, Ordering::SeqCst);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| t.next(Flip::default)));
    t.stop();
    // read once, unresolved: the same promise
    assert!(Arc::ptr_eq(&r.expect("next panicked").0, &p1.0));
}

/// RT2-13: the same for a repeating signal watcher.
#[test]
#[cfg_attr(miri, ignore)]
fn rt2_13_signal_next_with_a_promise_resolved_between_its_reads() {
    let _s = serial();
    let s: Signal<Flip> = Signal::new(28, true);
    let q0 = s.next(Flip::default).unwrap();
    q0.0 .1.store(true, Ordering::SeqCst);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.next(Flip::default)));
    let _ = s.stop();
    assert!(Arc::ptr_eq(&r.expect("next panicked").unwrap().0, &q0.0));
}
