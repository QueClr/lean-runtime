//! AR-40: a constant's keyed claim and its store, and a reference's keyed
//! take and its closing store, allocate nothing when no context waits: the
//! keyed tables (`sched::step_keyed`'s and `sched::ref_keyed`'s) keep their
//! first entries (8 and 4) in the thread-local itself. lean2rr's `main`
//! thread claims constants, and the first claim's allocation of a `Vec`
//! (160 bytes) shifted the layout of its heap (one more 2 MiB huge page at
//! the peak of a benchmark).
//!
//! This binary has a counting global allocator, which counts per thread.
//! Each check runs on a new thread, whose tables have no entry and have
//! never allocated, before the task manager runs (lean2rr's initializers
//! and `main` claim constants then). It counts the Rust global allocator
//! only (the allocator a translator configures, mimalloc in lean2rr), not
//! libc's `malloc`: glibc's `__cxa_thread_atexit_impl`, through which std
//! records a thread-local's destructor at its first use, is not seen.

use lean_runtime::sched::{done_keyed, ref_keyed, step_keyed};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    /// This thread's allocations so far: a `const` thread-local with no
    /// destructor, which the allocator can update at any time.
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
}

struct Counting;

// SAFETY: every call is forwarded to `System`, which upholds the contract;
// the count is a side effect that touches no allocation.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded under the caller's contract (`ptr` came from
        // `System` through this allocator).
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        // SAFETY: forwarded under the caller's contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// lean2rr's key of constant `slot` (odd).
fn key(slot: usize) -> usize {
    (slot << 1) | 1
}

/// The allocations `f` makes on a new thread.
fn allocs_on_a_new_thread(f: fn()) -> usize {
    std::thread::spawn(move || {
        let before = ALLOCS.with(Cell::get);
        f();
        ALLOCS.with(Cell::get) - before
    })
    .join()
    .unwrap()
}

#[test]
fn claims_and_takes_with_no_waiter_allocate_nothing() {
    // a constant's claim and its store, then 8 nested claims (a constant
    // whose initialization claims another), stored innermost first
    let claims = allocs_on_a_new_thread(|| {
        assert!(step_keyed(key(0)));
        done_keyed(key(0));
        for s in 1..=8 {
            assert!(step_keyed(key(s)));
        }
        for s in (1..=8).rev() {
            done_keyed(key(s));
        }
    });
    assert_eq!(claims, 0, "claims and stores");
    // a reference's take and closing store, then 4 nested (a `modify`
    // inside another's function)
    let takes = allocs_on_a_new_thread(|| {
        ref_keyed::take(64);
        assert!(ref_keyed::write_point());
        ref_keyed::store(64);
        for k in 1..=4 {
            ref_keyed::take(k * 64);
        }
        for k in (1..=4).rev() {
            assert!(ref_keyed::write_point());
            ref_keyed::store(k * 64);
        }
        assert!(!ref_keyed::write_point());
    });
    assert_eq!(takes, 0, "takes and closing stores");
}

/// Past their inline places the tables keep entries in a `Vec`, which
/// allocates: the count sees it.
#[test]
fn claims_and_takes_past_the_inline_places_allocate() {
    let claims = allocs_on_a_new_thread(|| {
        for s in 0..9 {
            assert!(step_keyed(key(s)));
        }
        for s in 0..9 {
            done_keyed(key(s));
        }
    });
    assert!(claims > 0, "9 nested claims");
    let takes = allocs_on_a_new_thread(|| {
        for k in 1..=5 {
            ref_keyed::take(k * 64);
        }
        for k in 1..=5 {
            ref_keyed::put(k * 64);
        }
    });
    assert!(takes > 0, "5 nested takes");
}
