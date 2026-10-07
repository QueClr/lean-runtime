//! Lean's stack-overflow report (`src/runtime/stack_overflow.cpp`) for the
//! scheduler's contexts and the threads that run them: an opt-in SIGSEGV and
//! SIGBUS handler, one implementation for both translators (AR-11), built
//! only with the feature `stack-overflow` (with `sched`, or with `threads`:
//! threads mode, `src/sched/threads.rs`, has no context, and every thread
//! its task manager makes calls [`install_stack_overflow_handler`] at its
//! entry). Without it, a task that overflows its context's stack ends with
//! a plain SIGSEGV (status 139), and the hub updates no record.
//!
//! This file holds one of the crate's `unsafe` items, a native quirk that no
//! safe API can reproduce: `UNSAFE.md` has its entry, and
//! `docs/native-quirks.md` ("Lean's stack-overflow report") its invariants
//! and proof. Every other file of `sched` stays free of `unsafe`.
//!
//! **What native does.** Every thread of a Lean program (the main thread,
//! each task manager worker: `lthread`'s `stack_guard`) has an alternate
//! signal stack, and the process has one handler for SIGSEGV and SIGBUS
//! (`SA_SIGINFO | SA_ONSTACK`), installed when the disposition is the
//! default. A fault whose address lies in the page below the faulting
//! thread's stack (`is_within_stack_guard`: `pthread_getattr_np`'s stack
//! address less one page) writes `\nStack overflow detected. Aborting.\n` to
//! file descriptor 2 and aborts (status 134; buffered output is lost). Any
//! other fault resets the disposition to the default and returns: the
//! faulting instruction runs again, and the default action ends the process
//! (status 139).
//!
//! **What the crate does.** Here a task runs on a context, a corosensei
//! coroutine whose stack has a guard page of its own. Neither Rust's handler
//! (std knows only the guards of threads) nor a glue's own handler knows
//! where the running context's guard is; the scheduler does. So:
//! - [`install_stack_overflow_handler`], the glue's one call, installs the
//!   handler process-wide (over Rust's, which it keeps as the previous
//!   action) and registers the calling thread; `sched::start` registers its
//!   thread too, once the handler is installed.
//! - Registering a thread gives it an alternate signal stack if it has none,
//!   and a record in an append-only table of chunks, which grows with the
//!   number of live registered threads: the thread's key (the address of its
//!   `errno`), the guard below its own stack, and the guard of the context
//!   running on it, which the hub updates at every switch ([`publish`]).
//!   When the thread ends, its record is freed. In threads mode only, the
//!   crate's alternate stack, if it gave one, is also disabled and kept on a
//!   free list for the next thread that needs one (review RT1-03); with
//!   `sched`, a registered thread ends only at the exit, so the block stays
//!   with it, as before RT1-03 (review AR-30: no `unsafe` that nothing
//!   needs).
//! - The handler finds the faulting thread's record by its key, with atomic
//!   loads only (no thread-local, no lock, no allocation), and reports Lean's
//!   message when the address lies in either guard.
//! - Any other fault goes to the previous action, called as the kernel
//!   would call it (its mask, `SA_NODEFER`, `SA_RESETHAND`): Rust's handler
//!   where Rust's runtime installed it (it reports an overflow of a Rust
//!   thread's own guard, else resets the default and returns), or, where
//!   the previous action was the default, the default restored, as Lean's
//!   handler does.

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use super::ctx::{running_stack, StackBounds};
use nix::libc::{self, c_int, c_void};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal};
use std::cell::Cell;
use std::mem::MaybeUninit;
use std::sync::atomic::{compiler_fence, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Once, OnceLock};

/// Lean's message (`segv_handler`, `stack_overflow.cpp`).
pub(crate) const MESSAGE: &[u8] = b"\nStack overflow detected. Aborting.\n";

/// The signals Lean's handler takes (`initialize_stack_overflow`).
const SIGNALS: [Signal; 2] = [Signal::SIGSEGV, Signal::SIGBUS];

/// The records of a chunk of the table (`Chunk`).
const CHUNK: usize = 64;

/// A record's key while its thread fills it (no `errno` lives at this
/// address).
const CLAIMING: usize = usize::MAX;

/// The bytes of an alternate signal stack the crate makes, on top of the
/// kernel's minimum for a signal frame (`AT_MINSIGSTKSZ`).
const ALTSTACK_EXTRA: usize = 64 << 10;

/// What the handler knows of one thread. Written only by that thread (I2 in
/// `docs/native-quirks.md`), read by its handler, and by that thread outside
/// the handler (`own_stack_low`).
struct Record {
    /// The thread's key: the address of its `errno`; 0 for a free record,
    /// `CLAIMING` while its thread fills it.
    key: AtomicUsize,
    /// The guard below the thread's own stack, `[thread_lo, thread_hi)`;
    /// `thread_lo` 0: none known.
    thread_lo: AtomicUsize,
    thread_hi: AtomicUsize,
    /// The guard of the context running on the thread, `[run_lo, run_hi)`;
    /// `run_lo` 0: `main`'s context (the thread's own stack).
    run_lo: AtomicUsize,
    run_hi: AtomicUsize,
}

impl Record {
    const fn free() -> Record {
        Record {
            key: AtomicUsize::new(0),
            thread_lo: AtomicUsize::new(0),
            thread_hi: AtomicUsize::new(0),
            run_lo: AtomicUsize::new(0),
            run_hi: AtomicUsize::new(0),
        }
    }

    /// Whether `addr` lies in one of the record's guards. The record is
    /// frozen while the handler runs: only its own thread writes it, and
    /// that thread is interrupted (I2, I3).
    fn covers(&self, addr: usize) -> bool {
        in_guard(
            self.thread_lo.load(Ordering::Relaxed),
            self.thread_hi.load(Ordering::Relaxed),
            addr,
        ) || in_guard(
            self.run_lo.load(Ordering::Relaxed),
            self.run_hi.load(Ordering::Relaxed),
            addr,
        )
    }

    /// Set a guard pair, `lo` last (0 first): a handler interrupting this
    /// on the same thread sees either no guard or a whole one (I3).
    fn set_pair(lo_a: &AtomicUsize, hi_a: &AtomicUsize, lo: usize, hi: usize) {
        lo_a.store(0, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        hi_a.store(hi, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        lo_a.store(lo, Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
    }

    /// The thread ends: the record becomes free.
    fn release(&self) {
        self.run_lo.store(0, Ordering::Relaxed);
        self.thread_lo.store(0, Ordering::Relaxed);
        self.key.store(0, Ordering::Release);
    }
}

/// Whether `addr` lies in the guard `[lo, hi)`; `lo` 0 is no guard.
fn in_guard(lo: usize, hi: usize, addr: usize) -> bool {
    lo != 0 && lo <= addr && addr < hi
}

/// A chunk of the table of records: its records, then the next chunk, made
/// when a thread finds every record taken (`claim`, outside the handler).
/// Append-only: a chunk is never freed or unlinked, so the table grows with
/// the number of live registered threads, without a bound (review RS3-01),
/// and the handler walks it with loads only (`OnceLock::get` never blocks
/// and allocates nothing).
struct Chunk {
    records: [Record; CHUNK],
    next: OnceLock<Box<Chunk>>,
}

impl Chunk {
    const fn new() -> Chunk {
        Chunk {
            records: [const { Record::free() }; CHUNK],
            next: OnceLock::new(),
        }
    }
}

/// The table's first chunk.
static TABLE: Chunk = Chunk::new();

/// The table's records, chunk after chunk. Loads only: async-signal-safe.
fn records() -> impl Iterator<Item = &'static Record> {
    std::iter::successors(Some(&TABLE), |c| c.next.get().map(|b| &**b))
        .flat_map(|c| c.records.iter())
}

/// The previous action of each of `SIGNALS`: its `sa_sigaction`, its
/// `sa_flags` (`SA_SIGINFO`: three arguments; `SA_RESETHAND`, `SA_NODEFER`)
/// and its `sa_mask` (bit `n - 1` for signal `n`, 1 to 64), written once,
/// before the handler is installed for that signal, and never again (I5).
static PREV: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static PREV_FLAGS: [AtomicUsize; 2] = [AtomicUsize::new(0), AtomicUsize::new(0)];
static PREV_MASK: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

static INSTALL: Once = Once::new();

thread_local! {
    /// This thread's record, if it has one. Read only outside the handler
    /// (`publish`, `register_thread`), and in the registration's
    /// destructor.
    static SLOT: Cell<Option<&'static Record>> = const { Cell::new(None) };
    /// The alternate signal stack the crate gave this thread
    /// (`ensure_altstack`): its block's address (exposed) and size. Threads
    /// mode only (review AR-30).
    #[cfg(feature = "threads")]
    static OWN_ALTSTACK: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    /// Frees the record, and in threads mode gives the crate's alternate
    /// stack back, when the thread ends (`Registration::drop`).
    static REGISTRATION: Registration = const { Registration };
}

struct Registration;

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(Some(r)) = SLOT.try_with(|s| s.replace(None)) {
            r.release();
        }
        #[cfg(feature = "threads")]
        if let Ok(Some((addr, size))) = OWN_ALTSTACK.try_with(Cell::take) {
            give_back_altstack(addr, size);
        }
    }
}

/// The alternate stacks of ended threads, for `ensure_altstack` to reuse
/// (review RT1-03): (address, size) of blocks the crate made. A block is
/// never freed; it is here only while no thread has it as its alternate
/// stack (I6). So the crate's blocks number at most the largest count of
/// threads alive at once with one of them, where threads mode would
/// otherwise leave one behind per ended thread (a thread per dedicated
/// task). Taken outside the handler only (registration, a thread's end).
/// Threads mode only (review AR-30): with `sched`, a registered thread ends
/// only at the exit.
#[cfg(feature = "threads")]
static FREE_ALTSTACKS: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());

#[cfg(feature = "threads")]
fn free_altstacks() -> std::sync::MutexGuard<'static, Vec<(usize, usize)>> {
    FREE_ALTSTACKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The thread ends (its `Registration`'s destructor): the block `addr` of
/// `size` bytes, its alternate stack from the crate, goes to the free list
/// once the kernel no longer uses it for this thread. If it is still the
/// thread's alternate stack, it is disabled first; if the thread runs on it
/// (never in a destructor) or the query fails, it is kept, as before RT1-03.
/// Threads mode only (review AR-30).
#[cfg(feature = "threads")]
fn give_back_altstack(addr: usize, size: usize) {
    let mut cur = MaybeUninit::<libc::stack_t>::uninit();
    // SAFETY: (U11) a null new stack only reads the current one into `cur`,
    // valid for a write of a `stack_t`.
    if unsafe { libc::sigaltstack(std::ptr::null(), cur.as_mut_ptr()) } != 0 {
        return;
    }
    // SAFETY: `sigaltstack` succeeded, so it wrote `cur`.
    let cur = unsafe { cur.assume_init() };
    if cur.ss_flags & libc::SS_DISABLE == 0 && cur.ss_sp.addr() == addr {
        if cur.ss_flags & libc::SS_ONSTACK != 0 {
            return;
        }
        let off = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: 0,
        };
        // SAFETY: (U14) disabling the calling thread's alternate stack, which
        // it does not run on (`SS_ONSTACK` clear): the kernel then delivers
        // no signal of this thread on the block.
        if unsafe { libc::sigaltstack(&off, std::ptr::null_mut()) } != 0 {
            return;
        }
    }
    free_altstacks().push((addr, size));
}

/// The calling thread's key: the address of its `errno`, which no other
/// live thread shares (std's handler keys its thread information the same
/// way). Async-signal-safe.
fn errno_location() -> *mut c_int {
    // SAFETY: (U1) `__errno_location` has no precondition; it returns the
    // address of the calling thread's `errno`, valid for the thread's life.
    unsafe { libc::__errno_location() }
}

/// The record of the thread whose key is `key`, if any. Atomic loads only.
fn record_of(key: usize) -> Option<&'static Record> {
    records().find(|r| r.key.load(Ordering::Acquire) == key)
}

/// Whether a fault at `addr` on the calling thread is a stack overflow:
/// `addr` lies in the guard of the thread's own stack or of the context
/// running on it.
fn overflow_at(addr: usize) -> bool {
    record_of(errno_location() as usize).is_some_and(|r| r.covers(addr))
}

/// The handler (`segv_handler` of `stack_overflow.cpp`), installed with
/// `SA_SIGINFO | SA_ONSTACK` for SIGSEGV and SIGBUS. It allocates nothing,
/// takes no lock and reads no thread-local of the crate (`docs/
/// native-quirks.md`, A1 to A4).
extern "C" fn on_fault(sig: c_int, info: *mut libc::siginfo_t, uctx: *mut c_void) {
    let errno = errno_location();
    // SAFETY: (U2) `errno` is the calling thread's `errno` (U1), valid and
    // aligned for reads and writes; the interrupted code holds no reference
    // to it (C code reads it through the same pointer).
    let saved = unsafe { *errno };
    // SAFETY: (U3) the kernel calls an `SA_SIGINFO` handler with a valid
    // `siginfo_t`; `si_addr` is the union member a kernel-generated SIGSEGV
    // or SIGBUS fills (`si_code > 0`), read only then.
    let (code, addr) = unsafe {
        let i = &*info;
        (
            i.si_code,
            if i.si_code > 0 {
                i.si_addr() as usize
            } else {
                0
            },
        )
    };
    if code > 0 && overflow_at(addr) {
        // SAFETY: (U4) `write(2)` of a static buffer of `MESSAGE.len()`
        // bytes to descriptor 2; async-signal-safe.
        unsafe { libc::write(libc::STDERR_FILENO, MESSAGE.as_ptr().cast(), MESSAGE.len()) };
        // `abort(3)`, async-signal-safe (std calls it and nothing else).
        std::process::abort();
    }
    forward(sig, info, uctx);
    // SAFETY: (U2) as above.
    unsafe { *errno = saved };
}

/// A fault that is no overflow goes to the previous action: its handler,
/// called as the kernel would call it (review SO-1), or the default
/// restored (Lean's handler: the faulting instruction runs again, and the
/// default action ends the process).
fn forward(sig: c_int, info: *mut libc::siginfo_t, uctx: *mut c_void) {
    let k = usize::from(sig != libc::SIGSEGV);
    let h = PREV[k].load(Ordering::Acquire);
    let flags = PREV_FLAGS[k].load(Ordering::Acquire) as c_int;
    let own = on_fault as extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) as usize;
    if h == libc::SIG_DFL || h == libc::SIG_IGN || h == own {
        restore_default(k);
        return;
    }
    // A one-shot handler: the kernel would have reset the disposition to the
    // default before calling it, so that the fault it does not repair runs
    // into the default action.
    if flags & libc::SA_RESETHAND != 0 {
        restore_default(k);
    }
    // The signals its action blocks while it runs, and the signal itself
    // unblocked under `SA_NODEFER`, as the kernel would set them.
    let mask = PREV_MASK[k].load(Ordering::Acquire);
    let mut saved = MaybeUninit::<libc::sigset_t>::uninit();
    let swapped = set_mask_for(sig, mask, flags & libc::SA_NODEFER != 0, saved.as_mut_ptr());
    if flags & libc::SA_SIGINFO != 0 {
        // SAFETY: (U6) `h` is the `sa_sigaction` the kernel recorded for this
        // signal before ours (I5), neither the default nor ignore, with
        // `SA_SIGINFO` set: so its owner installed a function with this
        // signature, which stays mapped (the program's own code).
        let f = unsafe {
            std::mem::transmute::<usize, extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void)>(h)
        };
        f(sig, info, uctx);
    } else {
        // SAFETY: (U6) as above, without `SA_SIGINFO`: a one-argument
        // handler.
        let f = unsafe { std::mem::transmute::<usize, extern "C" fn(c_int)>(h) };
        f(sig);
    }
    if swapped {
        // SAFETY: (U13) `saved` was written by the successful
        // `pthread_sigmask` of `set_mask_for`; async-signal-safe.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, saved.as_ptr(), std::ptr::null_mut()) };
    }
}

/// The default action for `SIGNALS[k]` (U5).
fn restore_default(k: usize) {
    let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
    // SAFETY: (U5) the default action is a valid disposition for SIGSEGV and
    // SIGBUS; `sigaction(2)` is async-signal-safe, and nix's wrapper only
    // fills a stack value around it.
    let _ = unsafe { nix::sys::signal::sigaction(SIGNALS[k], &dfl) };
}

/// Block the signals of `mask` (bit `n - 1` for signal `n`) and, with
/// `nodefer`, unblock `sig`, for a call of the previous handler; the mask
/// before goes to `saved`. Whether it changed the mask. Async-signal-safe:
/// `sigemptyset`, `sigaddset` and `pthread_sigmask` only (the last is
/// glibc's `rt_sigprocmask` system call).
fn set_mask_for(sig: c_int, mask: u64, nodefer: bool, saved: *mut libc::sigset_t) -> bool {
    if mask == 0 && !nodefer {
        return false;
    }
    let mut set = MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: (U13) `set` is a local `sigset_t`, initialized by
    // `sigemptyset` before `sigaddset` and `pthread_sigmask` read it;
    // `saved` points to the caller's local `sigset_t`, written on success.
    unsafe {
        libc::sigemptyset(set.as_mut_ptr());
        for n in 1..=64 {
            if mask & (1 << (n - 1)) != 0 {
                libc::sigaddset(set.as_mut_ptr(), n);
            }
        }
        if libc::pthread_sigmask(libc::SIG_BLOCK, set.as_ptr(), saved) != 0 {
            return false;
        }
        if nodefer {
            libc::sigemptyset(set.as_mut_ptr());
            libc::sigaddset(set.as_mut_ptr(), sig);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, set.as_ptr(), std::ptr::null_mut());
        }
    }
    true
}

/// Lean's stack-overflow report, for the scheduler's contexts and the
/// threads that run them (`src/runtime/stack_overflow.cpp`): the glue's one
/// call per OS thread that runs Lean code, at that thread's entry, before
/// any Lean code runs on it (the process's main thread for the
/// initializers, `main`'s thread if it is another one); the contexts need
/// nothing more, and `sched::start` registers its own thread too once this
/// has run. In threads mode every thread the task manager makes calls it at
/// its entry. Calling it again is harmless.
///
/// Call it from `main` (or later), after Rust's runtime has started, never
/// from an ELF constructor: std's runtime start installs Rust's handler,
/// and alternate stacks for the threads std spawns, only where it finds
/// the default disposition, so installed before it, this handler leaves a
/// Rust thread that does not register without Rust's report (review
/// SO-2). With a C-style entry, where std's runtime start never runs, the
/// previous action is the default.
///
/// Afterwards, on a registered thread, a SIGSEGV or SIGBUS whose address
/// lies in the guard page below the thread's own stack, or below the stack
/// of the context running on it, writes `\nStack overflow detected.
/// Aborting.\n` to descriptor 2 and aborts (status 134, buffered output
/// lost), as natively. Any other fault goes to the action that was there
/// before, called as the kernel would call it (its mask, `SA_NODEFER`,
/// `SA_RESETHAND`: review SO-1): Rust's handler, where Rust's runtime
/// installed it, reports the overflow of a Rust thread's own stack, else
/// restores the default; where it was the default, the default is restored
/// and the fault ends the process (status 139).
///
/// It installs one process-wide handler, over any handler already there
/// (Rust's), which it keeps as the previous action; a disposition set to
/// ignore is left alone. The calling thread gets an alternate signal stack
/// if it has none (a heap block, kept for the life of the process), and a
/// record (`docs/native-quirks.md`, "Lean's stack-overflow report"), in a
/// table that grows with the number of live registered threads; a record is
/// freed for reuse when its thread ends.
pub fn install_stack_overflow_handler() {
    register_thread();
    INSTALL.call_once(install_process_wide);
}

/// `sched::start`: register the calling thread if the handler is installed.
pub(crate) fn on_scheduler_thread() {
    if INSTALL.is_completed() {
        register_thread();
    }
}

/// Whether the calling thread has a record (tests).
#[cfg(test)]
pub(crate) fn registered() -> bool {
    SLOT.with(Cell::get).is_some()
}

/// The running context of the calling thread changes (the hub, at every
/// switch; `ctx::publish`): its record's context guard follows. Nothing on
/// a thread without a record.
pub(crate) fn publish(b: Option<StackBounds>) {
    let Some(r) = SLOT.with(Cell::get) else {
        return;
    };
    let (lo, hi) = b.map_or((0, 0), |b| (b.guard_lo, b.guard_hi));
    Record::set_pair(&r.run_lo, &r.run_hi, lo, hi);
}

/// The low end of the calling thread's own stack (the top of the guard
/// below it, `pthread_getattr_np`'s stack address), if the thread has a
/// record with a known guard: for the scheduler's room rule
/// (`ctx::stack_room`, hunt HSK-01). Not for the handler (a thread-local).
pub(crate) fn own_stack_low() -> Option<usize> {
    let r = SLOT.try_with(Cell::get).ok().flatten()?;
    if r.thread_lo.load(Ordering::Relaxed) == 0 {
        return None;
    }
    Some(r.thread_hi.load(Ordering::Relaxed))
}

fn install_process_wide() {
    let ours = SigAction::new(
        SigHandler::SigAction(on_fault),
        SaFlags::SA_ONSTACK,
        SigSet::empty(),
    );
    for (k, &sig) in SIGNALS.iter().enumerate() {
        let mut old = MaybeUninit::<libc::sigaction>::uninit();
        // SAFETY: (U7) a null new action only reads the current one into
        // `old`, valid for a write of a `sigaction`.
        if unsafe { libc::sigaction(sig as c_int, std::ptr::null(), old.as_mut_ptr()) } != 0 {
            continue;
        }
        // SAFETY: `sigaction` succeeded, so it wrote `old`.
        let old = unsafe { old.assume_init() };
        if old.sa_sigaction == libc::SIG_IGN {
            continue;
        }
        let mut mask = 0u64;
        for n in 1..=64 {
            // SAFETY: (U7) `old.sa_mask` was written by the successful
            // `sigaction` above; `sigismember` only reads it.
            if unsafe { libc::sigismember(&old.sa_mask, n) } == 1 {
                mask |= 1 << (n - 1);
            }
        }
        PREV_FLAGS[k].store(old.sa_flags as usize, Ordering::Release);
        PREV_MASK[k].store(mask, Ordering::Release);
        PREV[k].store(old.sa_sigaction, Ordering::Release);
        // SAFETY: (U8) `on_fault` is an `extern "C"` function with the
        // `SA_SIGINFO` signature that stays valid for the process's life, and
        // a sound handler for any SIGSEGV or SIGBUS at any point of the
        // program (A1 to A4 in `docs/native-quirks.md`).
        let _ = unsafe { nix::sys::signal::sigaction(sig, &ours) };
    }
}

/// Give the calling thread its alternate signal stack and its record.
fn register_thread() {
    if !matches!(SLOT.try_with(Cell::get), Ok(None)) {
        return;
    }
    ensure_altstack();
    let key = errno_location() as usize;
    let r = claim(key);
    let (tlo, thi) = own_guard().unwrap_or((0, 0));
    Record::set_pair(&r.thread_lo, &r.thread_hi, tlo, thi);
    let run = running_stack().map_or((0, 0), |b| (b.guard_lo, b.guard_hi));
    Record::set_pair(&r.run_lo, &r.run_hi, run.0, run.1);
    r.key.store(key, Ordering::Release);
    SLOT.with(|s| s.set(Some(r)));
    // Frees the record at the thread's end.
    let _ = REGISTRATION.try_with(|_| ());
}

/// A record for `key`: one it already has (left by a thread with the same
/// `errno` whose end did not free it), else a free one, in a new chunk if
/// every record is taken (review RS3-01).
fn claim(key: usize) -> &'static Record {
    if let Some(r) = record_of(key) {
        r.run_lo.store(0, Ordering::Relaxed);
        r.thread_lo.store(0, Ordering::Relaxed);
        return r;
    }
    let mut c: &'static Chunk = &TABLE;
    loop {
        if let Some(r) = c.records.iter().find(|r| {
            r.key
                .compare_exchange(0, CLAIMING, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        }) {
            return r;
        }
        c = c.next.get_or_init(|| Box::new(Chunk::new()));
    }
}

/// The guard below the calling thread's stack, `[lo, hi)`: Lean's
/// `is_within_stack_guard` (`pthread_getattr_np`'s stack address, less one
/// page), computed here, outside the handler, since `pthread_getattr_np`
/// is not async-signal-safe (it reads `/proc/self/maps` for the main
/// thread).
fn own_guard() -> Option<(usize, usize)> {
    let mut attr = MaybeUninit::<libc::pthread_attr_t>::zeroed();
    // SAFETY: (U9) `pthread_getattr_np` initializes `attr` with the calling
    // thread's attributes; `pthread_self` has no precondition.
    if unsafe { libc::pthread_getattr_np(libc::pthread_self(), attr.as_mut_ptr()) } != 0 {
        return None;
    }
    let mut addr: *mut c_void = std::ptr::null_mut();
    let mut size: libc::size_t = 0;
    // SAFETY: (U9) `attr` was initialized above; `addr` and `size` are valid
    // for writes.
    let rc = unsafe { libc::pthread_attr_getstack(attr.as_ptr(), &mut addr, &mut size) };
    // SAFETY: (U9) destroys the attributes initialized above, once.
    unsafe { libc::pthread_attr_destroy(attr.as_mut_ptr()) };
    if rc != 0 || addr.is_null() {
        return None;
    }
    let lo = addr as usize;
    Some((lo.checked_sub(page_size())?, lo))
}

fn page_size() -> usize {
    // SAFETY: (U10) `sysconf` has no precondition.
    let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    usize::try_from(p).unwrap_or(4096).max(4096)
}

/// Give the calling thread an alternate signal stack if it has none
/// (Lean's `stack_guard`; Rust gives its own threads one): a fault on an
/// overflowed stack is then delivered on it.
fn ensure_altstack() {
    let mut cur = MaybeUninit::<libc::stack_t>::uninit();
    // SAFETY: (U11) a null new stack only reads the current one into `cur`,
    // valid for a write of a `stack_t`.
    if unsafe { libc::sigaltstack(std::ptr::null(), cur.as_mut_ptr()) } != 0 {
        return;
    }
    // SAFETY: `sigaltstack` succeeded, so it wrote `cur`.
    let cur = unsafe { cur.assume_init() };
    if cur.ss_flags & libc::SS_DISABLE == 0 {
        return;
    }
    // SAFETY: (U10) `getauxval` has no precondition (0 when unknown).
    let min = unsafe { libc::getauxval(libc::AT_MINSIGSTKSZ) } as usize;
    let size = ALTSTACK_EXTRA + min.max(libc::SIGSTKSZ);
    // With `sched`, as before RT1-03: a new block for good (review AR-30).
    #[cfg(not(feature = "threads"))]
    {
        // A block no Rust reference ever points to: only the kernel writes it,
        // when it delivers a signal on it. Kept for the life of the process.
        let block: *mut [u8] = Box::into_raw(vec![0u8; size].into_boxed_slice());
        let ss = libc::stack_t {
            ss_sp: block.cast::<c_void>(),
            ss_flags: 0,
            ss_size: size,
        };
        // SAFETY: (U12) `ss` describes `size` bytes of a block that stays
        // allocated for the rest of the process and that nothing else reads or
        // writes; the thread does not run on an alternate stack now (none was
        // set).
        unsafe { libc::sigaltstack(&ss, std::ptr::null_mut()) };
    }
    // In threads mode: an ended thread's block, if one is free and large
    // enough (RT1-03); else a new one. A block no Rust reference ever points
    // to: only the kernel writes it, when it delivers a signal on it. Kept
    // for the life of the process.
    #[cfg(feature = "threads")]
    {
        let reused = {
            let mut free = free_altstacks();
            free.iter()
                .rposition(|&(_, s)| s >= size)
                .map(|k| free.swap_remove(k))
        };
        let (addr, size) = reused.unwrap_or_else(|| {
            let block: *mut [u8] = Box::into_raw(vec![0u8; size].into_boxed_slice());
            (block.cast::<u8>().expose_provenance(), size)
        });
        let ss = libc::stack_t {
            ss_sp: std::ptr::with_exposed_provenance_mut::<c_void>(addr),
            ss_flags: 0,
            ss_size: size,
        };
        // SAFETY: (U12) `ss` describes `size` bytes of a block that stays
        // allocated for the rest of the process and that nothing else reads or
        // writes: a new one, or one from the free list, which no thread has as
        // its alternate stack (I6); the thread does not run on an alternate
        // stack now (none was set).
        if unsafe { libc::sigaltstack(&ss, std::ptr::null_mut()) } == 0 {
            let _ = OWN_ALTSTACK.try_with(|c| c.set(Some((addr, size))));
        } else {
            free_altstacks().push((addr, size));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn guards_are_half_open_and_zero_is_none() {
        assert!(in_guard(0x1000, 0x2000, 0x1000));
        assert!(in_guard(0x1000, 0x2000, 0x1fff));
        assert!(!in_guard(0x1000, 0x2000, 0x2000));
        assert!(!in_guard(0x1000, 0x2000, 0xfff));
        assert!(!in_guard(0, 0x2000, 0x10));
    }

    #[test]
    fn a_record_covers_both_guards() {
        let r = Record::free();
        assert!(!r.covers(0x5000));
        Record::set_pair(&r.thread_lo, &r.thread_hi, 0x1000, 0x2000);
        Record::set_pair(&r.run_lo, &r.run_hi, 0x5000, 0x6000);
        assert!(r.covers(0x1800));
        assert!(r.covers(0x5000));
        assert!(!r.covers(0x6000));
        assert!(!r.covers(0x3000));
        // `main`'s context again: only the thread's own guard
        Record::set_pair(&r.run_lo, &r.run_hi, 0, 0);
        assert!(!r.covers(0x5000));
        assert!(r.covers(0x1000));
    }

    /// The tests that register threads run one at a time: a thread that ends
    /// frees its record, and a later thread may get the same `errno` address
    /// (glibc reuses a thread's stack and its thread-local block). Threads
    /// mode's tests take it too: every thread its task manager makes
    /// registers.
    pub(crate) static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// More live registered threads than a chunk holds (review RS3-01):
    /// each has its record, in a second chunk for the later ones, and each
    /// record is freed when its thread ends.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn more_threads_than_a_chunk_all_have_records() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let n = CHUNK + 6;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(n));
        let results: Vec<(usize, bool)> = (0..n)
            .map(|_| {
                let b = barrier.clone();
                std::thread::Builder::new()
                    .stack_size(256 << 10)
                    .spawn(move || {
                        register_thread();
                        // all registered and alive at once; no panic before
                        // the last wait, which every thread must reach
                        b.wait();
                        let key = errno_location() as usize;
                        let lo = own_guard().map_or(0, |g| g.0);
                        let ok = registered()
                            && record_of(key).is_some_and(|r| r.covers(lo))
                            && overflow_at(lo);
                        b.wait();
                        (key, ok)
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert!(
            results.iter().all(|&(_, ok)| ok),
            "every thread has its record"
        );
        assert!(TABLE.next.get().is_some(), "a second chunk");
        for (key, _) in results {
            assert!(record_of(key).is_none(), "freed at the thread's end");
        }
    }

    /// The base address of the calling thread's alternate signal stack, 0
    /// if it has none.
    #[cfg(feature = "threads")]
    fn current_altstack() -> usize {
        let mut cur = MaybeUninit::<libc::stack_t>::uninit();
        // SAFETY: test plumbing: a query into a valid `stack_t`.
        let rc = unsafe { libc::sigaltstack(std::ptr::null(), cur.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: written on success.
        let cur = unsafe { cur.assume_init() };
        if cur.ss_flags & libc::SS_DISABLE != 0 {
            0
        } else {
            cur.ss_sp.addr()
        }
    }

    /// As on a thread that std gave no alternate stack (a C-style entry,
    /// SIGSEGV ignored at the start, a foreign handler first): std's is
    /// disabled, then the thread registers and gets the crate's.
    #[cfg(feature = "threads")]
    fn register_without_std_altstack() -> usize {
        let off = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: 0,
        };
        // SAFETY: test plumbing: disabling the thread's alternate stack (std
        // unmaps its own block at the thread's end, whatever is current).
        let rc = unsafe { libc::sigaltstack(&off, std::ptr::null_mut()) };
        assert_eq!(rc, 0);
        assert_eq!(current_altstack(), 0);
        register_thread();
        let a = current_altstack();
        assert_ne!(a, 0, "the crate's alternate stack");
        a
    }

    /// Review RT1-03: the alternate stacks the crate makes are given back
    /// when their threads end, and reused: three threads alive at once,
    /// then five one after the other, use three blocks, not eight (threads
    /// mode makes a thread per dedicated task, so a block per ended thread
    /// would grow without bound; the review's example: 81,916 bytes per
    /// task).
    /// Threads mode only (review AR-30).
    #[test]
    #[cfg(feature = "threads")]
    #[cfg_attr(miri, ignore)]
    fn ended_threads_give_their_alternate_stacks_back() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut seen: std::collections::HashSet<usize> = (0..3)
            .map(|_| {
                let b = barrier.clone();
                std::thread::Builder::new()
                    .stack_size(256 << 10)
                    .spawn(move || {
                        let a = register_without_std_altstack();
                        // all three alive at once
                        b.wait();
                        a
                    })
                    .unwrap()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert_eq!(seen.len(), 3, "three live threads, three blocks");
        for _ in 0..5 {
            let a = std::thread::Builder::new()
                .stack_size(256 << 10)
                .spawn(register_without_std_altstack)
                .unwrap()
                .join()
                .unwrap();
            seen.insert(a);
        }
        assert_eq!(seen.len(), 3, "the blocks of ended threads are reused");
    }

    /// Registration and the publication of the running context, on a thread
    /// of its own (its record is freed when it ends). Installs no handler.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn a_registered_thread_has_its_guards() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        std::thread::spawn(|| {
            register_thread();
            assert!(registered());
            let key = errno_location() as usize;
            let r = record_of(key).expect("a record");
            let (lo, hi) = own_guard().expect("the thread's guard");
            assert!(r.covers(lo) && r.covers(hi - 1) && !r.covers(hi));
            publish(Some(StackBounds {
                guard_lo: 0x10000,
                guard_hi: 0x11000,
                top: 0x20000,
            }));
            assert!(overflow_at(0x10800));
            publish(None);
            assert!(!overflow_at(0x10800));
            assert!(overflow_at(lo));
            // the alternate signal stack is there
            let mut cur = MaybeUninit::<libc::stack_t>::uninit();
            // SAFETY: test plumbing: a query into a valid `stack_t`.
            let rc = unsafe { libc::sigaltstack(std::ptr::null(), cur.as_mut_ptr()) };
            assert_eq!(rc, 0);
            // SAFETY: written on success.
            let cur = unsafe { cur.assume_init() };
            assert_eq!(cur.ss_flags & libc::SS_DISABLE, 0);
            key
        })
        .join()
        .map(|key| assert!(record_of(key).is_none(), "freed at the thread's end"))
        .unwrap();
    }
}
