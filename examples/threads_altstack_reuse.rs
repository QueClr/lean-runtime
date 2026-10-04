//! Review RT1-03's check, run by `scripts/check.sh` (features `threads` and
//! `stack-overflow`): with a C-style entry, std's runtime start never runs,
//! so std gives its threads no alternate signal stack, and the crate's
//! stack-overflow report gives each thread the task manager makes one of
//! its own. Threads mode makes a thread per dedicated task; the blocks of
//! ended threads must be reused, not left behind (81,916 bytes per task on
//! aarch64 before the fix). It runs `N` dedicated tasks one after the other
//! and fails if the heap grew by more than a few blocks. Based on the
//! review's `examples/rt1_leak_c_main.rs`.
//!
//! A counting global allocator measures the heap: the only `unsafe` here,
//! test plumbing outside the crate.

#![no_main]

use lean_runtime::sched;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering::Relaxed};
use std::sync::Arc;

struct Counting;

/// The heap bytes allocated and not freed.
static LIVE: AtomicIsize = AtomicIsize::new(0);

// SAFETY: every call is forwarded to `System` with the same arguments; the
// counter changes nothing about the memory returned.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Relaxed);
        // SAFETY: the caller's contract is `System`'s.
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        LIVE.fetch_add(l.size() as isize, Relaxed);
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size() as isize, Relaxed);
        // SAFETY: as above.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_add(new as isize - l.size() as isize, Relaxed);
        // SAFETY: as above.
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

struct G;
impl sched::Glue for G {}

/// `n` dedicated tasks, one after the other: each runs on a thread of its
/// own, which ends.
fn dedicated_round(n: usize) {
    for _ in 0..n {
        let t = sched::spawn(Box::new(|| sched::Outcome::Done), 9, true);
        sched::wait(t);
        sched::release(t);
    }
}

/// The program, called from the C-style `main`.
fn run() -> i32 {
    sched::install_stack_overflow_handler();
    sched::start_with(Arc::new(G), 2, 256 << 10);
    // the first threads make the blocks the later ones reuse
    dedicated_round(20);
    std::thread::sleep(std::time::Duration::from_millis(50));
    let before = LIVE.load(Relaxed);
    const N: isize = 200;
    dedicated_round(N as usize);
    sched::finish();
    let grew = LIVE.load(Relaxed) - before;
    // a few blocks at most (threads still ending when `before` was read);
    // a block per task would be N times about 80 KiB
    let limit = 4 * 128 * 1024;
    println!("threads_altstack_reuse: {N} dedicated tasks: the heap grew by {grew} bytes (limit {limit})");
    if grew > limit {
        eprintln!("threads_altstack_reuse: the alternate stacks of ended threads are not reused");
        return 1;
    }
    0
}

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    run()
}
