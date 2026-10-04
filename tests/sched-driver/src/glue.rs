//! The glue a translator writes around `lean_runtime::sched`, as small as it
//! can be: the suspend step (its one `unsafe` block), Lean's stack-overflow
//! report for the scheduler's contexts, and the program's entry and exit.

use lean_runtime::sched::{self, CtxId, Glue, Suspend};
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct DriverGlue;

impl Glue for DriverGlue {
    fn suspend(&self, s: Suspend<'_>) {
        // SAFETY: lean-runtime calls `suspend` only on a context it started,
        // while that context runs, with that context's own yielder, which
        // corosensei keeps at the base of the context's stack until the
        // context's function returns (lean-runtime docs/sched.md, "Why
        // `Glue::suspend` is sound"). The pointer is not kept.
        unsafe { (*s.yielder()).suspend(()) }
    }

    fn switched(&self, _from: CtxId, _to: CtxId) {}
}

// ---------------------------------------------------------------------------
// Standard streams: stdout fully buffered (a pipe, as in the case runner),
// stderr unbuffered, as glibc's FILEs.

thread_local! {
    static STDOUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

const BUFSIZ: usize = 4096;

/// `IO.println`: an effect point, then glibc's buffered `fwrite`.
pub fn println(s: &str) {
    sched::effect();
    STDOUT.with(|b| {
        let mut b = b.borrow_mut();
        b.extend_from_slice(s.as_bytes());
        b.push(b'\n');
        if b.len() >= BUFSIZ {
            let _ = std::io::stdout().write_all(&b);
            let _ = std::io::stdout().flush();
            b.clear();
        }
    });
}

/// `IO.eprintln`: an effect point, then an unbuffered write.
pub fn eprintln(s: &str) {
    sched::effect();
    let mut e = std::io::stderr();
    let _ = e.write_all(s.as_bytes());
    let _ = e.write_all(b"\n");
}

/// A Lean panic's message (`lean_panic`, with `LEAN_BACKTRACE=0`): printed
/// through Lean's stderr stream, so an effect point like other output; the
/// program goes on.
pub fn lean_panic(msg: &str) {
    eprintln(msg);
}

/// `IO.Process.exit` (`lean_io_exit`): an effect point, then C's `exit`,
/// which flushes the standard streams but neither finalizes the task manager
/// nor waits for any task.
pub fn process_exit(code: u8) -> ! {
    sched::effect();
    flush_stdout();
    std::process::exit(code as i32)
}

fn flush_stdout() {
    STDOUT.with(|b| {
        let mut b = b.borrow_mut();
        let _ = std::io::stdout().write_all(&b);
        let _ = std::io::stdout().flush();
        b.clear();
    });
}

/// Lean's generated `main`: the initializers, `lean_init_task_manager`,
/// `main`, `lean_finalize_task_manager` (the final run, `sched::finish`),
/// then the flush of the standard streams at `exit` (decisions Q5
/// refinement A).
pub fn run(init: impl FnOnce(), main: impl FnOnce(&[String]) -> u32, args: &[String]) -> ! {
    install_stack_overflow_handler();
    init();
    sched::start(Rc::new(DriverGlue));
    // The cases are programs that create tasks (decisions Q5 refinement B).
    sched::set_ref_read_yields(true);
    let code = main(args);
    sched::finish();
    flush_stdout();
    std::process::exit(code as i32)
}

// ---------------------------------------------------------------------------
// Lean's stack-overflow report (`src/runtime/stack_overflow.cpp`): a fault
// in the guard page of the faulting thread's stack, here also of the running
// context's (`sched::running_stack`), prints the message and aborts.

/// The guard page below the main thread's stack, `[lo, hi)`.
static MAIN_GUARD_LO: AtomicUsize = AtomicUsize::new(0);
static MAIN_GUARD_HI: AtomicUsize = AtomicUsize::new(0);

extern "C" fn segv_handler(sig: libc::c_int, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
    // SAFETY: the kernel passes a valid `siginfo_t` to SA_SIGINFO handlers.
    let addr = unsafe { (*info).si_addr() } as usize;
    let (lo, hi) = (
        MAIN_GUARD_LO.load(Ordering::Relaxed),
        MAIN_GUARD_HI.load(Ordering::Relaxed),
    );
    let hit = match sched::running_stack() {
        Some(b) => b.guard_lo <= addr && addr < b.guard_hi,
        None => lo <= addr && addr < hi,
    };
    if hit {
        let msg = b"\nStack overflow detected. Aborting.\n";
        // SAFETY: write(2) and abort(3) are async-signal-safe.
        unsafe {
            libc::write(2, msg.as_ptr().cast(), msg.len());
            libc::abort();
        }
    }
    // Not an overflow: the default action, on return the fault repeats.
    // SAFETY: restoring the default disposition of `sig`.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
    }
}

fn install_stack_overflow_handler() {
    // SAFETY: plain libc calls on locally owned memory; the alternate signal
    // stack is leaked, so it outlives the handler.
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if libc::pthread_getattr_np(libc::pthread_self(), &mut attr) == 0 {
            let mut addr: *mut libc::c_void = std::ptr::null_mut();
            let mut size: libc::size_t = 0;
            libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
            libc::pthread_attr_destroy(&mut attr);
            let page = libc::sysconf(libc::_SC_PAGESIZE) as usize;
            MAIN_GUARD_LO.store((addr as usize).wrapping_sub(page), Ordering::Relaxed);
            MAIN_GUARD_HI.store(addr as usize, Ordering::Relaxed);
        }
        let alt = Box::leak(vec![0u8; 1 << 16].into_boxed_slice());
        let ss = libc::stack_t {
            ss_sp: alt.as_mut_ptr().cast(),
            ss_flags: 0,
            ss_size: alt.len(),
        };
        libc::sigaltstack(&ss, std::ptr::null_mut());
        for sig in [libc::SIGSEGV, libc::SIGBUS] {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = segv_handler as *const () as usize;
            act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}
