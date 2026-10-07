//! `Std.Internal.UV`'s event loop, timers and signals (Lean 4.34.0's
//! `src/runtime/uv/event_loop.cpp`, `timer.cpp`, `signal.cpp`, over libuv
//! 1.48) on the scheduler's event loop (`reactor`); feature `sched`.
//!
//! A failure is a libuv error code (a negative `errno`), which the glue
//! turns into Lean's error with `io::IoError::decode_uv_error(code, None)`,
//! as Lean's `lean_decode_uv_error(code, nullptr)`.
//!
//! Natively a dedicated thread runs libuv's loop (`event_loop_run_loop`), and
//! the externs lock it (`event_loop_lock`) to start, stop and restart its
//! handles; taking the lock makes the loop thread finish its iteration
//! first, so the timers due and the signals that arrived are handled before
//! the extern acts. A timer or a signal watcher resolves Lean promises from
//! that thread: the promise's waiters wake, and its `sync` dependents run
//! there. Here the loop's callbacks run on the scheduler's loop context
//! ([`super::timer_start`], [`super::watch`]), so a promise is resolved, and
//! its `sync` dependents run, on a context of its own as natively on a
//! thread of its own; and every extern first lets the loop catch up
//! (`reactor::catch_up`, review RSIOB-04).
//!
//! The promises are the translator's ([`LoopPromise`]): a handle's `next`
//! makes one through the glue (`lean_io_promise_new`), keeps a reference
//! (Lean's `m_promise`), and hands one to the program. Dropping a reference
//! is Lean's `lean_dec`: the glue resolves a promise whose last reference
//! goes with `none` (`deactivate_promise`), as natively.
//!
//! While a timer runs or a signal watcher listens, the loop holds the
//! handle (natively `lean_inc(obj)`): a running handle the program has
//! dropped still fires.

use super::reactor::catch_up;
use super::uv_signals::{native_signum, EINVAL};
use super::{timer_start, timer_stop, Interest, Ready, TimerId, WatchId};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The translator's `IO.Promise` as a timer or a signal watcher holds it: a
/// counted reference (cloning it is `lean_inc`, dropping it `lean_dec`).
pub trait LoopPromise: Clone + 'static {
    /// `promise_is_resolved`: the promise has a value.
    fn is_resolved(&self) -> bool;
    /// `lean_io_promise_resolve`: resolve it, if no one has, with `()` for a
    /// timer's promise (`IO.Promise Unit`; `value` is 0) or with the signal's
    /// number for a signal watcher's (`IO.Promise Int`). The glue calls
    /// `sched::resolve(id, || store)`; it runs on the loop context, where
    /// the promise's `sync` dependents run.
    fn resolve(&self, value: i64);
}

/// The state of a timer or a signal watcher (`TIMER_STATE_*`,
/// `SIGNAL_STATE_*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Initial,
    Running,
    Finished,
}

// ---------------------------------------------------------------------------
// The loop

/// `Std.Internal.UV.Loop.configure` (`lean_uv_event_loop_configure`).
/// Natively `accumulateIdleTime` turns on libuv's idle-time metrics
/// (`UV_METRICS_IDLE_TIME`) and `blockSigProfSignal` blocks `SIGPROF` in the
/// loop thread while it polls (`UV_LOOP_BLOCK_SIGNAL`); both succeed on
/// Linux, and nothing in Lean reads the metrics or sees the loop thread's
/// signal mask. Here the loop polls on the program's own thread, whose
/// `SIGPROF` must stay deliverable, so neither changes anything (case
/// `uvloop/loop_configure`). Never fails (a libuv error code otherwise).
pub fn loop_configure(accumulate_idle_time: bool, block_sigprof: bool) -> Result<(), i32> {
    super::ensure_started();
    catch_up();
    let _ = (accumulate_idle_time, block_sigprof);
    Ok(())
}

/// `Std.Internal.UV.Loop.alive` (`lean_uv_event_loop_alive`,
/// `uv_loop_alive`): true. Native Lean's loop always has its async handle
/// (`event_loop_init`), so it is always alive.
pub fn loop_alive() -> bool {
    super::ensure_started();
    catch_up();
    true
}

// ---------------------------------------------------------------------------
// Timers

struct TimerState<P: LoopPromise> {
    timeout: u64,
    repeating: bool,
    state: State,
    /// `m_promise`.
    promise: Option<P>,
    /// The loop's pending timer while it runs.
    armed: Option<TimerId>,
    /// Counts the starts of the loop's timer, so that a callback knows
    /// whether the timer was stopped or started again during its promise's
    /// `sync` dependents.
    starts: u64,
    /// The loop's reference to a running repeating timer with timeout 0
    /// after its tick (review HU-02): libuv does not start it again (repeat
    /// 0 is no repeat), so no armed callback holds it, but natively the
    /// loop keeps it (`lean_inc(obj)` at the start) until `stop`. A cycle,
    /// broken by `stop`.
    held: Option<Timer<P>>,
}

/// `Std.Internal.UV.Timer` (`lean_uv_timer_object`): a translator keeps one
/// in its external object (clones name the same timer).
pub struct Timer<P: LoopPromise>(Rc<RefCell<TimerState<P>>>);

impl<P: LoopPromise> Clone for Timer<P> {
    fn clone(&self) -> Self {
        Timer(self.0.clone())
    }
}

impl<P: LoopPromise> Timer<P> {
    /// `Timer.mk timeout repeating` (`lean_uv_timer_mk`): an initial timer.
    pub fn new(timeout: u64, repeating: bool) -> Timer<P> {
        super::ensure_started();
        catch_up();
        Timer::initial(timeout, repeating)
    }

    /// A placeholder for the glue only (a value to leave behind in a
    /// `mem::take`-style move), not Lean's `Timer.mk`: an initial one-shot
    /// timer with timeout 0, made without `catch_up`, so it never lets the
    /// loop or another context run (review AR-22). The glue must never give
    /// it to the program or use it as a timer; its externs act on it as on
    /// any initial timer, and doing so is the glue's error.
    pub fn placeholder() -> Timer<P> {
        Timer::initial(0, false)
    }

    fn initial(timeout: u64, repeating: bool) -> Timer<P> {
        Timer(Rc::new(RefCell::new(TimerState {
            timeout,
            repeating,
            state: State::Initial,
            promise: None,
            armed: None,
            starts: 0,
            held: None,
        })))
    }

    /// Start the loop's timer: due `after` ms from `from` (an extern's
    /// clock read, or the loop's time for the next period), then every
    /// `repeat` ms if `repeat` is not 0 (`uv_timer_start`, which counts
    /// from `loop->time`). A deadline past the clock's range never comes
    /// (libuv clamps it to `UINT64_MAX`).
    fn arm(&self, st: &mut TimerState<P>, from: Instant, after: u64, repeat: u64) {
        if let Some(id) = st.armed.take() {
            timer_stop(id);
        }
        st.starts += 1;
        let start = st.starts;
        let Some(deadline) = from.checked_add(Duration::from_millis(after)) else {
            return;
        };
        let me = self.clone();
        st.armed = Some(timer_start(
            deadline,
            Rc::new(move || me.fire(repeat, start)),
        ));
    }

    /// `handle_timer_event`, on the loop context.
    fn fire(&self, repeat: u64, start: u64) {
        let mut st = self.0.borrow_mut();
        if st.starts != start {
            return;
        }
        st.armed = None;
        if st.repeating {
            // libuv starts the next period (`uv_timer_again`) before it calls
            // back, from the loop's time (`loop->time` of the iteration, the
            // look that found the timer due: review HU-06, as threads mode
            // does); a repeat of 0 does not repeat, but the loop keeps the
            // running timer until `stop` (review HU-02)
            if st.state == State::Running {
                if repeat != 0 {
                    let from = super::reactor::loop_time().unwrap_or_else(Instant::now);
                    self.arm(&mut st, from, repeat, repeat);
                } else if st.held.is_none() {
                    st.held = Some(self.clone());
                }
            }
            let p = st.promise.clone().filter(|p| !p.is_resolved());
            drop(st);
            if let Some(p) = p {
                p.resolve(0);
            }
        } else {
            // `uv_timer_stop` and finished, the loop letting go of the timer,
            // before the promise resolves: a `sync` dependent of it sees a
            // finished timer, as every later dependent does (LB-20: natively
            // the timer is still running there, and the dependent's `stop` or
            // `cancel` releases it twice)
            st.state = State::Finished;
            let p = st.promise.clone();
            drop(st);
            if let Some(p) = p {
                p.resolve(0);
            }
        }
    }

    /// `Timer.next` (`lean_uv_timer_next`); `new_promise` is
    /// `lean_io_promise_new`.
    /// - One-shot: an initial timer starts and gives a new promise, which
    ///   resolves `timeout` ms later; a running or finished one gives that
    ///   promise again, or, after `stop`, a new one that never resolves.
    /// - Repeating: an initial timer starts and gives a new promise, which
    ///   resolves at once (the 0th multiple of `timeout`); a running one gives
    ///   its promise if it has not resolved, else a new one for the next
    ///   tick; a finished one gives its last promise, or, after `stop`, a new
    ///   one that never resolves.
    pub fn next(&self, new_promise: impl FnOnce() -> P) -> P {
        catch_up();
        let mut st = self.0.borrow_mut();
        match (st.state, st.repeating) {
            (State::Initial, _) => {
                let p = new_promise();
                st.promise = Some(p.clone());
                st.state = State::Running;
                let (after, repeat) = if st.repeating {
                    (0, st.timeout)
                } else {
                    (st.timeout, 0)
                };
                self.arm(&mut st, Instant::now(), after, repeat);
                p
            }
            (State::Running, true) => {
                if st.promise.as_ref().is_none_or(|p| p.is_resolved()) {
                    st.promise = Some(new_promise());
                }
                st.promise.clone().expect("set above")
            }
            _ => match &st.promise {
                Some(p) => p.clone(),
                None => {
                    drop(st);
                    new_promise()
                }
            },
        }
    }

    /// `Timer.reset` (`lean_uv_timer_reset`): a running timer's next
    /// resolution moves to `timeout` ms from now, then a repeating one ticks
    /// every `timeout` ms from there; otherwise nothing.
    pub fn reset(&self) {
        catch_up();
        let mut st = self.0.borrow_mut();
        if st.state == State::Running {
            let t = st.timeout;
            let repeat = if st.repeating { t } else { 0 };
            self.arm(&mut st, Instant::now(), t, repeat);
        }
    }

    /// `Timer.stop` (`lean_uv_timer_stop`): the timer lets go of its promise
    /// (which resolves only through the program's own references now); a
    /// running one stops and is finished.
    ///
    /// The state changes before the release, as on Lean master (PR #14793,
    /// in no release yet, not in 4.35.0-rc1): the loop's timer stopped, the
    /// promise taken out and the timer finished under the loop's lock, the
    /// promise released after it. The last reference resolves the promise
    /// with `none` and runs its `sync` dependents here: they see a finished
    /// timer, whose `next` gives a new promise that the timer does not hold,
    /// as after any `stop`. Lean 4.34.0 releases the promise first, while
    /// the timer still runs and stores it (timer.cpp 243-252): a dependent's
    /// `next` frees it twice and stores a promise that is then lost, or, on a
    /// one-shot timer, gets the promise being freed (LB-33). A timer that
    /// does not run lets go of its promise too, as in 4.34.0 (243-246;
    /// master returns early, 246-249).
    pub fn stop(&self) {
        catch_up();
        let (p, id, held) = {
            let mut st = self.0.borrow_mut();
            let p = st.promise.take();
            let id = if st.state == State::Running {
                st.state = State::Finished;
                st.armed.take()
            } else {
                None
            };
            // the loop lets go of a repeating timer with timeout 0
            // (`lean_dec(obj)`, review HU-02)
            (p, id, st.held.take())
        };
        if let Some(id) = id {
            timer_stop(id);
        }
        // dropped outside the borrow: the last reference resolves it with
        // `none`, which may run its `sync` dependents
        drop(p);
        drop(held);
    }

    /// `Timer.cancel` (`lean_uv_timer_cancel`): a running timer with a promise
    /// lets go of it; a one-shot one also stops and is initial again.
    ///
    /// As `stop`, the state changes before the release, as every later
    /// dependent sees it (native's outcome with `sync := false`): the `sync`
    /// dependents of the released promise see a running repeating timer
    /// without a promise, whose `next` gives one that the next tick
    /// resolves, or an initial one-shot timer, whose `next` starts it again.
    /// This order is lean-runtime's own correction: Lean master's `cancel`
    /// is unchanged (LB-33). Natively they see the timer as it was (271-284):
    /// a repeating timer's `next` frees the promise being released twice and
    /// stores a new one, which 274 overwrites without a release (it never
    /// resolves, though the timer ticks on), and a one-shot timer's `next`
    /// gives the promise being released (191-195 during 278: LB-33).
    pub fn cancel(&self) {
        catch_up();
        let (p, id) = {
            let mut st = self.0.borrow_mut();
            if st.state != State::Running || st.promise.is_none() {
                return;
            }
            let p = st.promise.take();
            let id = if st.repeating {
                None
            } else {
                st.state = State::Initial;
                st.armed.take()
            };
            (p, id)
        };
        if let Some(id) = id {
            timer_stop(id);
        }
        drop(p);
    }
}

// ---------------------------------------------------------------------------
// Signals

struct SignalState<P> {
    signum: i32,
    repeating: bool,
    state: State,
    promise: Option<P>,
    /// Listening (in this thread's watcher list).
    listening: bool,
    /// Counts the starts of its listening, so that a delivery knows whether
    /// a `sync` dependent stopped or started it again.
    starts: u64,
    /// Its creation order (the order of delivery among one signal's
    /// watchers of the same kind).
    seq: u64,
}

/// `Std.Internal.UV.Signal` (`lean_uv_signal_object`): a translator keeps
/// one in its external object (clones name the same watcher).
pub struct Signal<P: LoopPromise>(Rc<RefCell<SignalState<P>>>);

impl<P: LoopPromise> Clone for Signal<P> {
    fn clone(&self) -> Self {
        Signal(self.0.clone())
    }
}

/// A listening watcher, as this thread's list holds it (the loop's
/// reference).
trait Listener {
    /// `handle_signal_event`, on the loop context.
    fn deliver(&self);
}

impl<P: LoopPromise> Listener for Signal<P> {
    fn deliver(&self) {
        let st = self.0.borrow();
        if st.state != State::Running || !st.listening {
            return;
        }
        let (signum, start) = (st.signum, st.starts);
        if st.repeating {
            let p = st.promise.clone().filter(|p| !p.is_resolved());
            drop(st);
            if let Some(p) = p {
                p.resolve(signum as i64);
            }
        } else {
            // Finished before the promise resolves: a `sync` dependent of it
            // sees a finished watcher, as every later dependent does (LB-20:
            // natively it is still running there, and the dependent's `stop`
            // or `cancel` releases it twice). libuv calls back, then stops
            // listening (review RSIOB-11).
            drop(st);
            self.0.borrow_mut().state = State::Finished;
            let p = self.0.borrow().promise.clone();
            if let Some(p) = p {
                p.resolve(signum as i64);
            }
            if self.0.borrow().starts == start {
                self.unlisten();
            }
        }
    }
}

impl<P: LoopPromise> Signal<P> {
    /// `Signal.mk signum repeating` (`lean_uv_signal_mk`): an initial
    /// watcher of `signum` (Lean's number), or of no signal if Lean's table
    /// does not have it.
    pub fn new(signum: i32, repeating: bool) -> Signal<P> {
        super::ensure_started();
        catch_up();
        Signal::initial(native_signum(signum), repeating, signals::next_seq())
    }

    /// A placeholder for the glue only (a value to leave behind in a
    /// `mem::take`-style move), not Lean's `Signal.mk`: an initial one-shot
    /// watcher of no signal (`next` would fail with `UV_EINVAL`), made
    /// without `catch_up`, so it never lets the loop or another context run
    /// (review AR-22). The glue must never give it to the program or use it
    /// as a watcher; its externs act on it as on any initial watcher of no
    /// signal, and doing so is the glue's error.
    pub fn placeholder() -> Signal<P> {
        Signal::initial(0, false, 0)
    }

    fn initial(signum: i32, repeating: bool, seq: u64) -> Signal<P> {
        Signal(Rc::new(RefCell::new(SignalState {
            signum,
            repeating,
            state: State::Initial,
            promise: None,
            listening: false,
            starts: 0,
            seq,
        })))
    }

    /// The watcher's identity in this thread's list: its state's address.
    fn id(&self) -> usize {
        Rc::as_ptr(&self.0) as *const () as usize
    }

    fn unlisten(&self) {
        let (signum, repeating) = {
            let mut st = self.0.borrow_mut();
            if !st.listening {
                return;
            }
            st.listening = false;
            (st.signum, st.repeating)
        };
        signals::remove(self.id(), signum, !repeating);
    }

    /// `Signal.next` (`lean_uv_signal_next`); `new_promise` is
    /// `lean_io_promise_new`. As `Timer::next`: an initial watcher starts
    /// listening and gives a new promise, which resolves with the signal's
    /// number when the next one comes; a repeating one gives a new promise
    /// once the last has resolved. A watcher of no signal fails with
    /// `UV_EINVAL` there, and is left running with that promise, which
    /// never resolves (later `next`s give it), as natively. A failure is a
    /// libuv error code (`UV_EINVAL` is -22).
    pub fn next(&self, new_promise: impl FnOnce() -> P) -> Result<P, i32> {
        catch_up();
        let mut st = self.0.borrow_mut();
        match (st.state, st.repeating) {
            (State::Initial, _) => {
                let p = new_promise();
                st.promise = Some(p.clone());
                st.state = State::Running;
                st.starts += 1;
                let (signum, repeating, seq) = (st.signum, st.repeating, st.seq);
                drop(st);
                if signum == 0 {
                    return Err(-EINVAL);
                }
                signals::add(Rc::new(self.clone()), self.id(), seq, signum, !repeating)
                    .map_err(|e| -e)?;
                self.0.borrow_mut().listening = true;
                Ok(p)
            }
            (State::Running, true) => {
                if st.promise.as_ref().is_none_or(|p| p.is_resolved()) {
                    st.promise = Some(new_promise());
                }
                Ok(st.promise.clone().expect("set above"))
            }
            _ => match &st.promise {
                Some(p) => Ok(p.clone()),
                None => {
                    drop(st);
                    Ok(new_promise())
                }
            },
        }
    }

    /// `Signal.stop` (`lean_uv_signal_stop`): a running watcher stops
    /// listening, lets go of its promise and is finished. When no watcher of
    /// the signal is left, the signal has its default action again
    /// (`uv__signal_unregister_handler` restores `SIG_DFL`). Never fails
    /// (`uv_signal_stop` gives 0).
    ///
    /// The state changes before the release, as on Lean master (PR #14793,
    /// in no release yet, not in 4.35.0-rc1): the handle stopped, the
    /// promise taken out and the watcher finished under the loop's lock, the
    /// promise released after it. The `sync` dependents of the released
    /// promise see a finished watcher, whose `next` gives a new promise that
    /// the watcher does not hold. Lean 4.34.0 releases the promise first,
    /// while the watcher still runs and stores it (signal.cpp 236-241): a
    /// dependent's `next` frees it twice, and the process hangs at exit, or,
    /// on a one-shot watcher, gets the promise being freed (LB-34).
    pub fn stop(&self) -> Result<(), i32> {
        catch_up();
        let p = {
            let mut st = self.0.borrow_mut();
            if st.state != State::Running {
                return Ok(());
            }
            st.state = State::Finished;
            st.promise.take()
        };
        self.unlisten();
        drop(p);
        Ok(())
    }

    /// `Signal.cancel` (`lean_uv_signal_cancel`): a running watcher with a
    /// promise lets go of it; a one-shot one also stops listening and is
    /// initial again. As `stop`, the state changes before the release: a
    /// repeating watcher's `sync` dependent gets a promise from `next` that
    /// the watcher keeps and the next signal resolves; a one-shot watcher's
    /// starts listening again (natively it is lost, or the promise being
    /// freed: signal.cpp 263-273, LB-34). This order is lean-runtime's own
    /// correction: Lean master's `cancel` is unchanged.
    pub fn cancel(&self) {
        catch_up();
        let p = {
            let mut st = self.0.borrow_mut();
            if st.state != State::Running || st.promise.is_none() {
                return;
            }
            if !st.repeating {
                st.state = State::Initial;
            }
            st.promise.take()
        };
        if !self.0.borrow().repeating {
            self.unlisten();
        }
        drop(p);
    }
}

/// The signals' delivery on this thread. The process-wide part (each
/// signal's handlers through signal-hook's safe API, the loop's signal pipe,
/// the counts of listening watchers and libuv's re-registration) is
/// `super::uv_signals`, shared with threads mode; here are this thread's
/// listening watchers and the loop's watch of the pipe's read end, whose
/// call delivers the signals that came to this thread's watchers of them.
mod signals {
    use super::super::uv_signals::{arrived, listen, unlisten};
    use super::{Interest, Listener, Ready, WatchId};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// One listening watcher of this thread.
    struct Entry {
        signum: i32,
        repeating: bool,
        /// Creation order (libuv's signal tree orders a signal's watchers by
        /// handle address, that is creation order, after putting repeating
        /// ones first: review RSIOB-08).
        seq: u64,
        /// Its identity (`Signal::id`).
        id: usize,
        watcher: Rc<dyn Listener>,
    }

    /// This thread's listening watchers and the loop's watch of the pipe.
    #[derive(Default)]
    struct Delivery {
        /// The loop's watch of the pipe's read end, while a watcher of this
        /// thread listens.
        watch: Option<WatchId>,
        listeners: Vec<Entry>,
    }

    impl Drop for Delivery {
        fn drop(&mut self) {
            // At thread exit the watchers are not dropped: they hold Lean
            // values (a promise, whose release resolves it and runs its
            // `sync` dependents) and would call back into the destroyed
            // scheduler (review RSIOB-01; natively nothing is destructed at
            // exit).
            for e in self.listeners.drain(..) {
                std::mem::forget(e.watcher);
            }
        }
    }

    thread_local! {
        static DELIVERY: RefCell<Delivery> = RefCell::new(Delivery::default());
    }

    pub(super) use super::super::uv_signals::next_seq;

    /// A watcher of `signum` (identity `id`, creation order `seq`) starts
    /// listening (`uv_signal_start`, `uv_signal_start_oneshot`).
    pub(super) fn add(
        watcher: Rc<dyn Listener>,
        id: usize,
        seq: u64,
        signum: i32,
        oneshot: bool,
    ) -> Result<(), i32> {
        let read = listen(signum, oneshot)?;
        DELIVERY.with(|d| {
            let mut d = d.borrow_mut();
            d.listeners.push(Entry {
                signum,
                repeating: !oneshot,
                seq,
                id,
                watcher,
            });
            if d.watch.is_none() {
                d.watch = super::super::watch(read, Interest::READ, Rc::new(on_ready)).ok();
            }
        });
        Ok(())
    }

    /// A watcher of `signum` (identity `id`) stops listening
    /// (`uv_signal_stop`).
    pub(super) fn remove(id: usize, signum: i32, oneshot: bool) {
        let (gone, watch) = DELIVERY.with(|d| {
            let mut d = d.borrow_mut();
            let Some(i) = d.listeners.iter().position(|e| e.id == id) else {
                return (None, None);
            };
            let e = d.listeners.remove(i);
            let w = if d.listeners.is_empty() {
                d.watch.take()
            } else {
                None
            };
            (Some(e.watcher), w)
        });
        if let Some(w) = watch {
            super::super::unwatch(w);
        }
        if gone.is_some() {
            unlisten(signum, oneshot);
        }
        drop(gone);
    }

    /// The loop's call: drain the pipe, then deliver each signal that came
    /// (in signal-number order: occurrences of one signal that came between
    /// two calls are one delivery, where libuv makes one per occurrence) to
    /// this thread's watchers of it, repeating ones first, then in creation
    /// order. The watchers of every signal of the batch are taken before
    /// the first delivery (review HU-05): natively the handler writes one
    /// message per watcher listening when the signal comes, so a watcher
    /// that a `sync` dependent of an earlier delivery starts does not get
    /// a signal of the same batch.
    fn on_ready(_: Ready) {
        let came = arrived();
        let batch: Vec<Vec<Rc<dyn Listener>>> = DELIVERY.with(|d| {
            let d = d.borrow();
            came.iter()
                .map(|&s| {
                    let mut ls: Vec<&Entry> =
                        d.listeners.iter().filter(|e| e.signum == s).collect();
                    ls.sort_by_key(|e| (!e.repeating, e.seq));
                    ls.into_iter().map(|e| e.watcher.clone()).collect()
                })
                .collect()
        });
        for ls in batch {
            for l in ls {
                l.deliver();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sched::{self, Glue, Suspend, TaskId};
    use std::cell::Cell;

    struct NoSuspend;
    impl Glue for NoSuspend {
        fn suspend(&self, _: Suspend<'_>) {
            panic!("the crate's unit tests never suspend a context");
        }
    }

    /// A translator's promise: the scheduler's task and a slot.
    #[derive(Clone)]
    struct P(Rc<(TaskId, Cell<Option<i64>>)>);

    impl P {
        fn new() -> P {
            P(Rc::new((sched::promise_new().unwrap(), Cell::new(None))))
        }
        fn get(&self) -> Option<i64> {
            if self.0 .1.get().is_none() {
                sched::wait(self.0 .0);
            }
            self.0 .1.get()
        }
        fn same(&self, o: &P) -> bool {
            Rc::ptr_eq(&self.0, &o.0)
        }
    }

    impl LoopPromise for P {
        fn is_resolved(&self) -> bool {
            self.0 .1.get().is_some()
        }
        fn resolve(&self, v: i64) {
            let me = self.clone();
            sched::resolve(self.0 .0, move || me.0 .1.set(Some(v)));
        }
    }

    fn start() {
        sched::start_with(Rc::new(NoSuspend), 2, 1 << 20);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_one_shot_timer_resolves_once_and_keeps_its_promise() {
        start();
        let t: Timer<P> = Timer::new(20, false);
        let p = t.next(P::new);
        assert!(!p.is_resolved());
        assert!(t.next(P::new).same(&p));
        assert_eq!(p.get(), Some(0));
        assert!(t.next(P::new).same(&p));
        t.stop();
        let q = t.next(P::new);
        assert!(!q.same(&p) && !q.is_resolved());
        sched::finish();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_repeating_timer_ticks_at_once_then_every_period() {
        // Each extern catches the loop up, so a check that a tick has not
        // happened yet holds only while less than a period has passed: the
        // period is 100 ms, and after a host stall past the tick's earliest
        // time such a check is skipped, with a note (review RS2-11 of
        // sched-2). The first period starts at the first `next`, after `t0`,
        // and each one lasts at least `PERIOD`.
        const PERIOD: u64 = 100;
        let before = |t0: Instant, periods: u64, what: &str| {
            let on_time = t0.elapsed() < Duration::from_millis(PERIOD * periods);
            if !on_time {
                eprintln!("note: the host stalled past tick {periods}: {what} is not checked");
            }
            on_time
        };
        start();
        let t: Timer<P> = Timer::new(PERIOD, true);
        let t0 = Instant::now();
        let p = t.next(P::new);
        assert_eq!(p.get(), Some(0));
        let a = t.next(P::new);
        assert!(!a.same(&p));
        let again = t.next(P::new);
        if before(t0, 1, "the same promise until it resolves") {
            assert!(again.same(&a), "the same promise until it resolves");
        }
        assert_eq!(a.get(), Some(0));
        assert!(t0.elapsed() >= Duration::from_millis(PERIOD));
        // cancel drops the promise; the timer ticks on
        let b = t.next(P::new);
        t.cancel();
        let b_dropped_unresolved = before(t0, 2, "the cancel before the second tick");
        let c = t.next(P::new);
        assert!(!c.same(&b));
        assert_eq!(c.get(), Some(0));
        if b_dropped_unresolved {
            assert!(!b.is_resolved());
        }
        t.stop();
        sched::finish();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_cancelled_one_shot_timer_starts_anew() {
        // `cancel` catches the loop up first: it drops `p` unresolved only if
        // it comes before the timeout (100 ms; after a host stall past it,
        // the timer may have fired first, and only the consistency of `p`
        // and the next promise is checked, with a note; review RS2-11).
        const TIMEOUT: u64 = 100;
        start();
        let t: Timer<P> = Timer::new(TIMEOUT, false);
        let t0 = Instant::now();
        let p = t.next(P::new);
        t.cancel();
        let on_time = t0.elapsed() < Duration::from_millis(TIMEOUT);
        if !on_time {
            eprintln!(
                "note: the host stalled past the timeout: the cancel before it is not checked"
            );
        }
        // After a stall the timer may have fired first: then the cancel did
        // nothing, and the finished timer gives `p` again.
        let q = t.next(P::new);
        let fired = q.same(&p);
        if on_time {
            assert!(!fired, "a new promise after the cancel");
        }
        assert_eq!(q.get(), Some(0));
        assert_eq!(p.is_resolved(), fired);
        sched::finish();
    }

    /// A translator's promise as a counted reference (the glue's): the last
    /// reference to go resolves it with `none` (`deactivate_promise`). The
    /// slot holds `Some(value)` once it has resolved.
    #[derive(Clone)]
    struct Q(Rc<QObj>);

    struct QObj {
        id: TaskId,
        slot: Rc<Cell<Option<Option<i64>>>>,
    }

    impl Drop for QObj {
        fn drop(&mut self) {
            let slot = self.slot.clone();
            sched::resolve(self.id, move || slot.set(Some(None)));
        }
    }

    impl Q {
        fn new() -> Q {
            Q(Rc::new(QObj {
                id: sched::promise_new().unwrap(),
                slot: Rc::new(Cell::new(None)),
            }))
        }
    }

    impl LoopPromise for Q {
        fn is_resolved(&self) -> bool {
            self.0.slot.get().is_some()
        }
        fn resolve(&self, v: i64) {
            let slot = self.0.slot.clone();
            sched::resolve(self.0.id, move || slot.set(Some(Some(v))));
        }
    }

    /// What a re-subscribing dependent saw: the values, the promises it
    /// subscribed to, and in each run whether two `next`s gave the same
    /// promise (one the timer holds) or two new ones (a finished timer).
    #[derive(Clone, Default)]
    struct Seen {
        values: Rc<RefCell<Vec<Option<i64>>>>,
        promises: Rc<RefCell<Vec<TaskId>>>,
        held: Rc<RefCell<Vec<bool>>>,
    }

    /// `arm` of case `uvloop/timer_oneshot_stop_resubscribe`: `next`, then a
    /// `sync` dependent of the promise that subscribes again, whatever the
    /// value, at most `cap` times; the promise is dropped at once, as
    /// compiled Lean drops it after `result?`.
    fn subscribe(t: &Timer<Q>, seen: &Seen, cap: usize) {
        let p = t.next(Q::new);
        seen.promises.borrow_mut().push(p.0.id);
        let (t2, seen2, slot) = (t.clone(), seen.clone(), p.0.slot.clone());
        sched::depend(
            p.0.id,
            Box::new(move || {
                seen2
                    .values
                    .borrow_mut()
                    .push(slot.get().expect("resolved"));
                let (a, b) = (t2.next(Q::new), t2.next(Q::new));
                seen2.held.borrow_mut().push(Rc::ptr_eq(&a.0, &b.0));
                drop((a, b));
                if seen2.values.borrow().len() < cap {
                    subscribe(&t2, &seen2, cap);
                }
                sched::Outcome::Done
            }),
            0,
            true,
            true,
        );
    }

    /// Each run of the dependent saw `none` and a finished timer (no promise
    /// held), and every promise it subscribed to has read `none`.
    fn ran_on_a_finished_timer(seen: &Seen, cap: usize) {
        assert_eq!(*seen.values.borrow(), vec![None; cap]);
        assert_eq!(*seen.held.borrow(), vec![false; cap]);
        assert_eq!(seen.promises.borrow().len(), cap);
        assert!(seen
            .promises
            .borrow()
            .iter()
            .all(|&p| sched::is_finished(p)));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_stopped_timer_is_finished_before_its_promise_is_released() {
        // LB-33: as on Lean master (PR #14793), `stop` finishes the timer,
        // then releases the promise. The `sync` dependent that the release
        // runs sees a finished timer: its `next` gives a promise the timer
        // does not hold, which reads `none` when dropped, so a dependent that
        // subscribes again runs again, up to its cap (natively, 4.34.0, it
        // ran once and its new promise was lost).
        start();
        let t: Timer<Q> = Timer::new(3_600_000, true);
        // the 0th tick resolves at once; the next one is an hour away
        let p0 = t.next(Q::new);
        if p0.0.slot.get().is_none() {
            sched::wait(p0.0.id);
        }
        assert_eq!(p0.0.slot.get(), Some(Some(0)));
        let seen = Seen::default();
        subscribe(&t, &seen, 5);
        assert!(seen.values.borrow().is_empty());
        t.stop();
        ran_on_a_finished_timer(&seen, 5);
        sched::finish();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_stopped_one_shot_timer_is_finished_before_its_promise_is_released() {
        // The same for a one-shot timer stopped before it fires (natively the
        // dependent's `next` gives the promise being released, which has
        // read `none`: the same runs, through a use after free).
        start();
        let t: Timer<Q> = Timer::new(3_600_000, false);
        let seen = Seen::default();
        subscribe(&t, &seen, 5);
        assert!(seen.values.borrow().is_empty());
        t.stop();
        ran_on_a_finished_timer(&seen, 5);
        sched::finish();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_placeholder_lets_nothing_run() {
        // AR-22: `Timer::new` and `Signal::new` catch the loop up, so a due
        // timer's `sync` dependent runs there; a placeholder runs nothing.
        start();
        let t: Timer<Q> = Timer::new(1, false);
        let p = t.next(Q::new);
        let ran = Rc::new(Cell::new(false));
        let r2 = ran.clone();
        sched::depend(
            p.0.id,
            Box::new(move || {
                r2.set(true);
                sched::Outcome::Done
            }),
            0,
            true,
            true,
        );
        // due now, but nothing has looked at the loop yet
        std::thread::sleep(Duration::from_millis(5));
        let _a: Timer<Q> = Timer::placeholder();
        let _b: Signal<Q> = Signal::placeholder();
        assert!(!ran.get(), "a placeholder ran the due timer's dependent");
        assert!(!p.is_resolved());
        // the check sees a catch-up: `Timer::new` runs it
        let _c: Timer<Q> = Timer::new(1, false);
        assert!(ran.get());
        assert_eq!(p.0.slot.get(), Some(Some(0)));
        sched::finish();
    }

    /// Review HU-02 (fixes-14): after the tick of a repeating timer with
    /// timeout 0, no armed callback holds it, but the loop does
    /// (`lean_inc(obj)` at its start), until `stop`.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_repeating_timer_with_timeout_zero_is_held_until_stop() {
        start();
        let t: Timer<P> = Timer::new(0, true);
        let p0 = t.next(P::new);
        assert_eq!(p0.get(), Some(0));
        assert!(t.0.borrow().armed.is_none());
        assert_eq!(Rc::strong_count(&t.0), 2, "the loop holds the timer");
        let p1 = t.next(P::new);
        assert!(!p1.same(&p0) && !p1.is_resolved());
        t.stop();
        assert_eq!(Rc::strong_count(&t.0), 1, "stop lets go of it");
        sched::finish();
    }

    /// Review HU-06 (fixes-14): a repeating timer's next period starts from
    /// the look that found the tick due (libuv's `loop->time`), not from
    /// when the loop context runs its callback.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_repeating_timers_next_period_starts_at_the_look() {
        start();
        let t: Timer<P> = Timer::new(100, true);
        let p0 = t.next(P::new);
        assert_eq!(p0.get(), Some(0));
        let p1 = t.next(P::new);
        // tick 1 comes due while nothing looks
        std::thread::sleep(Duration::from_millis(120));
        let look = Instant::now();
        crate::sched::with(|s| {
            s.ev_check(look, true);
            s.ev_start_loop();
        });
        // the loop context runs the callback 30 ms after the look
        std::thread::sleep(Duration::from_millis(30));
        sched::poll();
        assert!(p1.is_resolved());
        assert_eq!(
            crate::sched::with(|s| s.ev.next_timer()),
            Some(look + Duration::from_millis(100))
        );
        t.stop();
        sched::finish();
    }

    #[test]
    fn an_unknown_signal_is_refused_at_next() {
        start();
        let s: Signal<P> = Signal::new(99, false);
        let e = s.next(P::new).err().expect("refused");
        assert_eq!(e, -22);
        // left running with that promise, as natively
        let p = s.next(P::new).unwrap();
        assert!(s.next(P::new).unwrap().same(&p));
        assert!(!p.is_resolved());
        s.stop().unwrap();
    }
}
