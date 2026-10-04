//! The process title in the arguments' memory: libuv 1.48's `uv_setup_args`
//! and the write of `uv_set_process_title` (`src/unix/proctitle.c`), which
//! Lean 4.34.0 reaches through `lean_setup_args` (the generated `main`, before
//! the module initializers) and `Std.Internal.UV.System.setProcessTitle`.
//!
//! This file holds one of the crate's `unsafe` items, a native quirk that no
//! safe API can reproduce: `UNSAFE.md` has its entry, and
//! `docs/native-quirks.md` ("The process title in the arguments' memory")
//! its invariants and proof. It is compiled only with the feature
//! `proc-title`; without it, `setProcessTitle` fails with `UV_ENOBUFS` and
//! `getProcessTitle` gives `argv[0]` ([`super::uvsys`]), and the crate root
//! forbids `unsafe`. Every other file of the crate stays free of `unsafe`:
//! the crate root denies it, and `scripts/check.sh` fails on a file that
//! allows it without an entry in `UNSAFE.md`.
//!
//! **What native does.**
//! - `uv_setup_args(argc, argv)`: with `argc > 0`, the title's storage is
//!   `argv[0]`'s memory, and its capacity `cap` is every byte from `argv[0]`
//!   to the NUL that ends `argv[argc - 1]`: `argv[argc - 1] +
//!   strlen(argv[argc - 1]) + 1 - argv[0]`. libuv copies the strings and the
//!   pointer table to a block of its own (`args_mem`), and the generated
//!   `main` builds Lean's `args` from that copy, so a later title does not
//!   change `args`. With `argc <= 0` it keeps nothing, and the title
//!   functions fail with `UV_ENOBUFS`.
//! - `uv_set_process_title(title)`: a title of `cap` bytes or more is cut to
//!   `cap - 1` bytes; the title goes to the start of the memory (`memcpy`)
//!   and the rest of the `cap` bytes become NUL (`memset`), which wipes the
//!   other arguments. Then `prctl(PR_SET_NAME)` ([`super::uvsys`]).
//! - libuv does not move the environment (which the kernel puts right after
//!   the arguments), so no title is ever longer than `cap - 1` bytes: a
//!   longer one is cut, without an error.
//! - The kernel's `/proc/<pid>/cmdline` reads that memory (`arg_start` to
//!   `arg_end`), so it shows the title followed by NUL bytes, and so do `ps`
//!   and glibc's `program_invocation_name`, which points to `argv[0]`.
//!
//! **What the crate does.** On glibc, an ELF constructor of this file gets
//! `main`'s `argc` and `argv` before `main` (glibc calls each `.init_array`
//! function with `argc`, `argv` and `envp`), so no translator's glue takes
//! part. It does what `uv_setup_args` does: it computes `cap` as libuv does,
//! keeps the memory's start and `cap`, copies `argv[0]` as the first title,
//! copies every argument into a block of its own (leaked, as libuv's
//! `args_mem`) and points the table at the copies. So `main`'s `argv`, glibc's
//! `__libc_argv` and `std::env::args` give the original arguments for good,
//! as Lean's `args` are natively, and no safe code reaches the arguments'
//! memory. Each `setProcessTitle` then writes that memory exactly as libuv's
//! `memcpy` and `memset` leave it ([`write`]).
//!
//! In a shared library (also one loaded by `dlopen` after `main`) the
//! constructor reads nothing and keeps "no arguments", so the title functions
//! fail with `ENOBUFS`, as natively, where only a program's generated `main`
//! calls `uv_setup_args`: a running host may own its arguments' memory. A
//! program started through the dynamic loader (`ld.so ./prog`) reads as a
//! library and gets `ENOBUFS` too, where native writes the title: the
//! judged deviation LQ1-01 (`docs/native-quirks.md`).
//!
//! In the program's executable, the constructor keeps nothing, and
//! [`super::uvsys`] models the title from `std::env::args_os` without
//! writing (the title, its cut and the thread's name are native's;
//! `/proc/self/cmdline` keeps the arguments), when it cannot check what it
//! would write: another thread already runs, `/proc/self` cannot be read, or the
//! memory from `argv[0]` to the end of `argv[argc - 1]` is not inside the
//! kernel's span of the arguments (`arg_start` and `arg_end` in
//! `/proc/self/stat`: an earlier constructor moved an entry of the table).
//! With `argc <= 0` it keeps nothing, as natively, and the title functions
//! fail with `ENOBUFS`.

#![allow(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::ffi::{c_char, c_int, CStr};
use std::sync::atomic::AtomicPtr;
use std::sync::{Mutex, PoisonError};

/// The arguments' memory, as `uv_setup_args` keeps it (libuv's
/// `process_title`).
///
/// Invariant (R): `start` is valid for writes of `cap` bytes for the rest of
/// the process, `cap >= 1`, and nothing outside this file accesses those
/// bytes while this file writes them. Only [`Region::from_argv`], an `unsafe
/// fn` whose contract gives R, makes a `Region` (`docs/native-quirks.md`,
/// I1 to I7).
struct Region {
    /// `argv[0]`: the first byte. An `AtomicPtr` only so that `Region` is
    /// `Send` (and [`REGION`] a valid `static`) without an `unsafe impl`; it
    /// is read through `get_mut`, under [`REGION`]'s lock.
    start: AtomicPtr<u8>,
    /// libuv's `pt.cap`.
    cap: usize,
    /// `argv[0]`'s bytes, copied in [`setup_args`]: the title before any
    /// `setProcessTitle`.
    first: Vec<u8>,
}

/// What [`setup_args`] kept: `None` before it ran, or when it could not check
/// what it would write; `Some(None)` when there were no arguments (libuv's
/// `args_mem` stays null); else the region. Every access to the arguments'
/// memory from this file holds this lock.
static REGION: Mutex<Option<Option<Region>>> = Mutex::new(None);

impl Region {
    /// libuv's `uv_setup_args`, without the copy: the memory from `argv[0]`
    /// to the NUL ending `argv[argc - 1]`. `None` with `argc <= 0`, as
    /// libuv keeps nothing then, and when the memory is not inside `span`
    /// (`arg_start`, `arg_end`: the kernel's span of the arguments), or does
    /// not start with `argv[0]` and its NUL.
    ///
    /// # Safety
    ///
    /// - **C1.** If `argc > 0`, `argv` is valid and aligned for reading
    ///   `argc` pointers.
    /// - **C2.** `argv[0]` and `argv[argc - 1]`, when not null, point to
    ///   NUL-terminated strings, and the bytes of `span` are valid for reads
    ///   and writes for as long as the `Region` is used.
    /// - **C3.** While the `Region` is used, no code outside this file
    ///   accesses the bytes of `span` at the same time as this file, and no
    ///   reference into them is live across [`Region::write`].
    unsafe fn from_argv(
        argc: c_int,
        argv: *const *mut c_char,
        span: (usize, usize),
    ) -> Option<Region> {
        if argc <= 0 || argv.is_null() {
            return None;
        }
        // `argc > 0`, so it fits `usize`
        let last_index = argc as usize - 1;
        // SAFETY: (U1) C1: `argv` is valid and aligned for reading `argc`
        // pointers, and indices 0 and `argc - 1` are below `argc`.
        let (first, last) = unsafe { (*argv, *argv.add(last_index)) };
        if first.is_null() || last.is_null() {
            return None;
        }
        // SAFETY: (U2) C2: `argv[0]` points to a NUL-terminated string,
        // readable, which nothing writes during this call (C3, and this
        // file holds `REGION`'s lock, which every write of its own holds).
        let first_bytes = unsafe { CStr::from_ptr(first) }.to_bytes().to_vec();
        // SAFETY: (U2) as above, for `argv[argc - 1]`.
        let last_len = unsafe { CStr::from_ptr(last) }.to_bytes().len();
        // libuv: `pt.cap = argv[argc - 1] + strlen(argv[argc - 1]) + 1 - argv[0]`
        let end = last.addr().checked_add(last_len)?.checked_add(1)?;
        let cap = end.checked_sub(first.addr())?;
        // the memory starts with `argv[0]` and its NUL, and lies inside the
        // kernel's span (on a normal `execve`, it is exactly the span)
        if cap <= first_bytes.len() || first.addr() < span.0 || end > span.1 {
            return None;
        }
        Some(Region {
            start: AtomicPtr::new(first.cast::<u8>()),
            cap,
            first: first_bytes,
        })
    }

    /// `uv_set_process_title`'s write: `title` cut to `cap - 1` bytes, then
    /// NUL bytes to the end of the memory. Returns the length kept.
    fn write(&mut self, title: &[u8]) -> usize {
        // libuv: `if (len >= pt->cap) len = pt->cap - 1` (`cap >= 1` by R)
        let len = title.len().min(self.cap - 1);
        // the memory as libuv's `memcpy` and `memset` leave it
        let mut image = vec![0u8; self.cap];
        image[..len].copy_from_slice(&title[..len]);
        let start = *self.start.get_mut();
        // SAFETY: (U3) by R, `start` is valid for writes of `cap` bytes and
        // nothing else accesses them now (C3; this file holds `REGION`'s
        // lock, as `&mut self` comes from it). `image` is valid for reads of
        // `cap` bytes, and it cannot overlap the region: it is a fresh
        // allocation, and the region is live memory it does not own. `u8`
        // has no alignment requirement.
        unsafe { std::ptr::copy_nonoverlapping(image.as_ptr(), start, self.cap) };
        len
    }
}

/// libuv's `args_mem`: copies every argument into one block, which it never
/// frees, and points `argv`'s entries at the copies (null entries stay
/// null). Returns the block, for a test to free.
///
/// # Safety
///
/// - **C4.** `argv` is valid and aligned for reading and writing `argc`
///   pointers, each null or pointing to a NUL-terminated string, and no other
///   thread runs.
unsafe fn copy_and_repoint(argc: usize, argv: *mut *mut c_char) -> *mut [u8] {
    let mut block = Vec::new();
    let mut offsets = Vec::with_capacity(argc);
    for i in 0..argc {
        // SAFETY: (U1) C4: `i < argc`.
        let p = unsafe { *argv.add(i) };
        if p.is_null() {
            offsets.push(None);
            continue;
        }
        offsets.push(Some(block.len()));
        // SAFETY: (U2) C4: a NUL-terminated string, and no thread writes it.
        block.extend_from_slice(unsafe { CStr::from_ptr(p) }.to_bytes_with_nul());
    }
    let block: *mut [u8] = Box::into_raw(block.into_boxed_slice());
    let base = block.cast::<u8>();
    for (i, offset) in offsets.into_iter().enumerate() {
        if let Some(o) = offset {
            // SAFETY: `o` is the start of a copy inside the block, which lives
            // until a test frees it (never, outside the tests).
            let copy = unsafe { base.add(o) }.cast::<c_char>();
            // SAFETY: (U4) C4: `i < argc`, the table is valid for writes, and
            // no other thread reads it.
            unsafe { *argv.add(i) = copy };
        }
    }
    block
}

/// `arg_start` and `arg_end` of `/proc/self/stat` (fields 48 and 49): where
/// the kernel put the arguments' strings. Read with safe code.
fn kernel_arg_span() -> Option<(usize, usize)> {
    arg_span_of(&std::fs::read("/proc/self/stat").ok()?)
}

/// The fields after `comm` of a `/proc/<pid>/stat` line, from field 3 on
/// (`comm` may hold spaces and `)`: it ends at the last `)`).
fn stat_fields(stat: &[u8]) -> Vec<&[u8]> {
    let rest = match stat.iter().rposition(|&b| b == b')') {
        Some(at) => &stat[at + 1..],
        None => &[][..],
    };
    rest.split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
        .collect()
}

/// Field `n` (from 1) of a `/proc/<pid>/stat` line, as a number.
fn stat_field(stat: &[u8], n: usize) -> Option<usize> {
    let f = *stat_fields(stat).get(n.checked_sub(3)?)?;
    std::str::from_utf8(f).ok()?.parse().ok()
}

fn arg_span_of(stat: &[u8]) -> Option<(usize, usize)> {
    let (start, end) = (stat_field(stat, 48)?, stat_field(stat, 49)?);
    (start < end).then_some((start, end))
}

/// The kernel's `PF_IO_WORKER` task flag (`/proc/<pid>/task/<tid>/stat`,
/// field 9): an io_uring kernel thread, which runs no code of the process.
/// The flag and such threads in `/proc/<pid>/task` exist from Linux 5.12;
/// before 5.5, the same bit was `PF_VCPU`.
const PF_IO_WORKER: usize = 0x10;

/// The running kernel's major and minor version, from `uname`'s release
/// (`7.0.0-1019-nvidia` gives `(7, 0)`).
fn kernel_major_minor() -> Option<(u32, u32)> {
    major_minor_of(rustix::system::uname().release().to_bytes())
}

fn major_minor_of(release: &[u8]) -> Option<(u32, u32)> {
    let mut parts = release.split(|&b| b == b'.');
    let mut number = || -> Option<u32> {
        let p = parts.next()?;
        let digits = p.iter().take_while(|b| b.is_ascii_digit()).count();
        std::str::from_utf8(&p[..digits]).ok()?.parse().ok()
    };
    Some((number()?, number()?))
}

/// Whether no other thread of the process runs code of the process: every
/// task in `/proc/self/task` but this one is an io_uring kernel thread (such
/// as the polling ring's, when the startup descriptors are already open).
/// io_uring kernel threads are told apart only on Linux 5.12 or later
/// (review of quirks-1); on an older kernel any other task counts.
fn no_other_thread() -> bool {
    let io_workers_known = kernel_major_minor().is_some_and(|v| v >= (5, 12));
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return false;
    };
    let mut others = 0;
    for t in tasks {
        let Ok(t) = t else { return false };
        let Ok(stat) = std::fs::read(t.path().join("stat")) else {
            return false;
        };
        match stat_field(&stat, 9) {
            Some(flags) if io_workers_known && flags & PF_IO_WORKER != 0 => {}
            Some(_) => others += 1,
            None => return false,
        }
    }
    others == 1
}

/// Whether `code`, an address in this crate's code, lies in the program's
/// own executable rather than in a shared library: inside the kernel's
/// record of the program's code, `start_code` to `end_code`
/// (`/proc/self/stat`, fields 26 and 27; leanrs's review of quirks-1:
/// comparing the device and inode of `/proc/self/maps` and
/// `/proc/self/exe` fails on btrfs and older overlayfs). `None` when that
/// cannot be told (no `/proc`, the fields hidden).
///
/// A program started through the dynamic loader (`ld.so ./prog`) reads as
/// not in the executable: the kernel's record is then the loader's code. That
/// launch keeps "no arguments" (`ENOBUFS`), where native writes the title: a
/// judged deviation (LQ1-01, `docs/native-quirks.md`).
fn in_main_executable(code: usize) -> Option<bool> {
    code_span_of(&std::fs::read("/proc/self/stat").ok()?)
        .map(|(start, end)| (start..end).contains(&code))
}

/// `start_code` and `end_code` of a `/proc/<pid>/stat` line (fields 26 and
/// 27); `None` when they are missing, or hidden from a reader without
/// permission (the kernel then shows both as 1: an empty span).
fn code_span_of(stat: &[u8]) -> Option<(usize, usize)> {
    let (start, end) = (stat_field(stat, 26)?, stat_field(stat, 27)?);
    (start < end).then_some((start, end))
}

/// libuv's `uv_setup_args`, called once by the constructor below: checks what
/// it would write, then keeps the region and repoints the table (see the
/// module comment). `code` is the constructor's address.
///
/// # Safety
///
/// - **C5.** `argc` and `argv` are the ones glibc passes to the
///   `.init_array` functions, and `code` is the address of the function glibc
///   called. glibc calls the program's own `.init_array` functions before
///   `main`, a library's when it loads it (also after `main`, by `dlopen`);
///   so nothing is read or written unless `code` lies in the program's own
///   executable, where this runs before `main`.
unsafe fn setup_args(argc: c_int, argv: *mut *mut c_char, code: usize) {
    let mut g = REGION.lock().unwrap_or_else(PoisonError::into_inner);
    if g.is_some() {
        return;
    }
    if argc <= 0 || argv.is_null() {
        *g = Some(None);
        return;
    }
    match in_main_executable(code) {
        // the crate is in the program's executable: its constructor runs
        // before `main`
        Some(true) => {}
        // in a shared library: natively nothing calls `uv_setup_args` there,
        // and the title functions fail with `ENOBUFS`; nothing is read
        Some(false) => {
            *g = Some(None);
            return;
        }
        None => return,
    }
    if !no_other_thread() {
        return;
    }
    let Some(span) = kernel_arg_span() else {
        return;
    };
    // SAFETY: by C5 (the program's own constructor, before `main`) and
    // glibc's and the kernel's conventions (`docs/native-quirks.md`,
    // "Proof"): C1, as glibc's table holds `argc` pointers; C2, as the
    // kernel's span of the arguments is on the stack it mapped for the
    // process, which stays mapped; C3, as no other thread runs now, the
    // table is repointed below, and `REGION` keeps the region for the rest
    // of the process.
    let Some(region) = (unsafe { Region::from_argv(argc, argv, span) }) else {
        return;
    };
    // SAFETY: C4: glibc's table holds `argc` pointers to the arguments, on the
    // writable stack, and no other thread runs (checked above).
    let _ = unsafe { copy_and_repoint(argc as usize, argv) };
    *g = Some(Some(region));
}

/// The ELF constructor: glibc calls each `.init_array` function with `main`'s
/// `argc`, `argv` and `envp` (`call_init` in `csu/libc-start.c` for the
/// program, `_dl_init` for a library); musl calls them with none, hence
/// glibc only. Not under Miri, which has no process arguments' memory.
#[cfg(all(target_os = "linux", target_env = "gnu", not(miri)))]
mod constructor {
    use std::ffi::{c_char, c_int};

    extern "C" fn hand_in_arguments(argc: c_int, argv: *mut *mut c_char, _envp: *mut *mut c_char) {
        // SAFETY: C5: glibc calls `.init_array` functions with the
        // process's `argc` and `argv` (`main`'s), and `hand_in_arguments` is
        // the function it called; `setup_args` reads nothing unless this
        // function is in the program's own executable, whose constructors
        // run before `main`. Nothing in `setup_args` unwinds but a failed
        // allocation, which aborts; an unwind could not leave this
        // `extern "C"` function anyway (Rust aborts).
        unsafe { super::setup_args(argc, argv, (hand_in_arguments as *const ()).addr()) }
    }

    /// Referred to by [`super::keep_constructor`].
    #[used]
    #[link_section = ".init_array"]
    pub(super) static HAND_IN_ARGUMENTS: extern "C" fn(c_int, *mut *mut c_char, *mut *mut c_char) =
        hand_in_arguments;
}

/// Refers to the constructor, so that the linker keeps the object that holds
/// it wherever the title functions are linked (a `#[used]` static alone
/// does not keep its object file, from an rlib or an archive).
#[inline]
fn keep_constructor() {
    #[cfg(all(target_os = "linux", target_env = "gnu", not(miri)))]
    std::hint::black_box(&constructor::HAND_IN_ARGUMENTS);
}

/// What the title starts as, by the constructor's [`setup_args`].
pub(crate) enum Setup {
    /// It kept nothing it could check: the title is modelled from
    /// `std::env::args_os`, and nothing is written.
    NotCalled,
    /// It ran with no arguments: `uv_setup_args` with `argc <= 0`.
    NoArguments,
    /// `argv[0]` (the title `uv_setup_args` starts with) and libuv's `cap`.
    Arguments(Vec<u8>, usize),
}

/// What [`setup_args`] kept.
pub(crate) fn initial() -> Setup {
    keep_constructor();
    let g = REGION.lock().unwrap_or_else(PoisonError::into_inner);
    match &*g {
        None => Setup::NotCalled,
        Some(None) => Setup::NoArguments,
        Some(Some(r)) => Setup::Arguments(r.first.clone(), r.cap),
    }
}

/// Write `title` into the arguments' memory as `uv_set_process_title` does
/// (cut to `cap - 1` bytes, then NUL bytes to the end), if the constructor
/// kept it: `Some` of the length kept, `None` otherwise.
pub(crate) fn write(title: &[u8]) -> Option<usize> {
    keep_constructor();
    let mut g = REGION.lock().unwrap_or_else(PoisonError::into_inner);
    g.as_mut().and_then(Option::as_mut).map(|r| r.write(title))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the kernel's layout: the strings one after another in
    /// one block, with the environment after them, and a pointer table.
    struct Args {
        block: Vec<u8>,
        offsets: Vec<usize>,
        /// The arguments' bytes (the kernel's `arg_end - arg_start`).
        len: usize,
    }

    impl Args {
        fn new(args: &[&str], env: &str) -> Args {
            let mut block = Vec::new();
            let mut offsets = Vec::new();
            for a in args {
                offsets.push(block.len());
                block.extend_from_slice(a.as_bytes());
                block.push(0);
            }
            let len = block.len();
            block.extend_from_slice(env.as_bytes());
            block.push(0);
            Args {
                block,
                offsets,
                len,
            }
        }

        /// A region over the block, with the arguments' bytes as the
        /// kernel's span, used and dropped by `f` before the block is read
        /// again.
        fn with_region<R>(&mut self, f: impl FnOnce(Option<&mut Region>) -> R) -> R {
            let base = self.block.as_mut_ptr();
            let table: Vec<*mut c_char> = self
                .offsets
                .iter()
                // SAFETY: each offset is within `block`.
                .map(|&o| unsafe { base.add(o) }.cast::<c_char>())
                .collect();
            let argc = c_int::try_from(table.len()).unwrap();
            let span = (base.addr(), base.addr() + self.len);
            // SAFETY: `table` holds `argc` pointers to NUL-terminated strings
            // laid out one after another in `block`, which holds `span`,
            // outlives the region, and which nothing else touches until `f`
            // returns.
            let mut r = unsafe { Region::from_argv(argc, table.as_ptr(), span) };
            f(r.as_mut())
        }
    }

    #[test]
    fn cap_and_first_title_are_libuvs() {
        let mut a = Args::new(&["./prog", "ab", "c"], "HOME=/x");
        a.with_region(|r| {
            let r = r.unwrap();
            assert_eq!(r.first, b"./prog");
            // 7 + 3 + 2 bytes: every argument with its NUL, not the environment
            assert_eq!(r.cap, 12);
        });
        let mut one = Args::new(&["p"], "E=1");
        one.with_region(|r| assert_eq!(r.unwrap().cap, 2));
        let mut empty = Args::new(&[""], "");
        empty.with_region(|r| assert_eq!(r.unwrap().cap, 1));
    }

    #[test]
    fn no_arguments_keep_nothing() {
        // SAFETY: with `argc <= 0` nothing is read.
        assert!(unsafe { Region::from_argv(0, std::ptr::null(), (0, 1)) }.is_none());
        // SAFETY: as above.
        assert!(unsafe { Region::from_argv(-1, std::ptr::null(), (0, 1)) }.is_none());
    }

    /// Layouts no process is given keep nothing, and nothing is written: a
    /// null `argv[0]`, and a last argument before the first.
    #[test]
    fn impossible_layouts_keep_nothing() {
        let table: [*mut c_char; 1] = [std::ptr::null_mut()];
        // SAFETY: `table` holds one pointer; a null one is not read through.
        assert!(unsafe { Region::from_argv(1, table.as_ptr(), (0, usize::MAX)) }.is_none());
        let mut block = *b"first\0b\0";
        let base = block.as_mut_ptr();
        let span = (base.addr(), base.addr() + block.len());
        // SAFETY: offsets 6 and 0 are within `block`.
        let table = unsafe { [base.add(6).cast::<c_char>(), base.cast::<c_char>()] };
        // SAFETY: `table` holds two pointers to NUL-terminated strings in
        // `block`, which outlives the call and is not written.
        assert!(unsafe { Region::from_argv(2, table.as_ptr(), span) }.is_none());
        assert_eq!(&block, b"first\0b\0");
    }

    /// The memory must lie inside the kernel's span of the arguments: an
    /// entry an earlier constructor pointed elsewhere (here, the last
    /// argument past the span) keeps nothing, and nothing is written.
    #[test]
    fn memory_outside_the_kernels_span_keeps_nothing() {
        let mut block = *b"prog\0ab\0E=1\0moved\0";
        let base = block.as_mut_ptr();
        // the kernel's span: `prog` and `ab`
        let span = (base.addr(), base.addr() + 8);
        // SAFETY: offsets 0 and 12 are within `block`.
        let table = unsafe { [base.cast::<c_char>(), base.add(12).cast::<c_char>()] };
        // SAFETY: `table` holds two pointers to NUL-terminated strings in
        // `block`, which outlives the call and is not written.
        assert!(unsafe { Region::from_argv(2, table.as_ptr(), span) }.is_none());
        // the same table inside a span that holds it is kept
        let wide = (base.addr(), base.addr() + block.len());
        // SAFETY: as above; the region is dropped at once.
        let r = unsafe { Region::from_argv(2, table.as_ptr(), wide) }.unwrap();
        assert_eq!(r.cap, 18);
        drop(r);
        // a span that starts after `argv[0]` keeps nothing either
        let late = (base.addr() + 1, base.addr() + block.len());
        // SAFETY: as above.
        assert!(unsafe { Region::from_argv(2, table.as_ptr(), late) }.is_none());
        assert_eq!(&block, b"prog\0ab\0E=1\0moved\0");
    }

    /// As libuv, `cap` runs from `argv[0]` to the end of the last argument,
    /// whatever the table points to in between: here it skips a string,
    /// which the title then covers. (Such a table comes from a launch through
    /// the dynamic loader, `ld.so --argv0`, which the crate does not write
    /// for: LQ1-01. The rule is libuv's all the same.)
    #[test]
    fn cap_spans_a_skipped_string() {
        let mut block = *b"a\0skip\0b\0E=1\0";
        let base = block.as_mut_ptr();
        let span = (base.addr(), base.addr() + 9);
        // SAFETY: offsets 0 and 7 are within `block`.
        let table = unsafe { [base.cast::<c_char>(), base.add(7).cast::<c_char>()] };
        // SAFETY: `table` holds two pointers to NUL-terminated strings, and
        // the 9 bytes from the first to the end of the second are in
        // `block`, which outlives the region and which nothing else touches
        // until it is dropped.
        let mut r = unsafe { Region::from_argv(2, table.as_ptr(), span) }.unwrap();
        assert_eq!((r.first.as_slice(), r.cap), (&b"a"[..], 9));
        assert_eq!(r.write(b"0123456789"), 8);
        drop(r);
        assert_eq!(&block, b"01234567\0E=1\0");
    }

    #[test]
    fn write_copies_then_fills_with_nul() {
        let mut a = Args::new(&["./prog", "ab", "c"], "HOME=/x");
        let kept = a.with_region(|r| r.unwrap().write(b"new"));
        assert_eq!(kept, 3);
        assert_eq!(&a.block[..12], b"new\0\0\0\0\0\0\0\0\0");
        // the environment is not touched
        assert_eq!(&a.block[12..], b"HOME=/x\0");
    }

    #[test]
    fn a_long_title_is_cut_to_cap_less_one() {
        let mut a = Args::new(&["./prog", "ab", "c"], "HOME=/x");
        let kept = a.with_region(|r| r.unwrap().write(b"0123456789abcdef"));
        assert_eq!(kept, 11);
        assert_eq!(&a.block[..12], b"0123456789a\0");
        assert_eq!(&a.block[12..], b"HOME=/x\0");
        // exactly `cap` bytes is cut too; `cap - 1` is kept whole
        let mut b = Args::new(&["abc"], "");
        assert_eq!(b.with_region(|r| r.unwrap().write(b"wxyz")), 3);
        assert_eq!(b.block, b"wxy\0\0");
        let mut c = Args::new(&["abc"], "");
        assert_eq!(c.with_region(|r| r.unwrap().write(b"")), 0);
        assert_eq!(c.block, b"\0\0\0\0\0");
    }

    /// libuv's `args_mem`: the table points at copies afterwards, so a title
    /// written over the original memory leaves the arguments as they were.
    #[test]
    fn the_table_points_at_copies() {
        let mut a = Args::new(&["./prog", "ab", "c"], "HOME=/x");
        let base = a.block.as_mut_ptr();
        let span = (base.addr(), base.addr() + a.len);
        let mut table: Vec<*mut c_char> = a
            .offsets
            .iter()
            // SAFETY: each offset is within `block`.
            .map(|&o| unsafe { base.add(o) }.cast::<c_char>())
            .collect();
        table.push(std::ptr::null_mut());
        // SAFETY: `table` holds three pointers to NUL-terminated strings in
        // `block` (and a null one), which outlives the region; nothing else
        // touches the block until the region is dropped.
        let mut r = unsafe { Region::from_argv(3, table.as_ptr(), span) }.unwrap();
        // SAFETY: `table` is valid for reads and writes of 4 pointers, each
        // null or a NUL-terminated string; one thread.
        let copies = unsafe { copy_and_repoint(4, table.as_mut_ptr()) };
        r.write(b"title-over-everything");
        drop(r);
        let args: Vec<&[u8]> = table[..3]
            .iter()
            // SAFETY: each entry points into `copies`, which is still live.
            .map(|&p| unsafe { CStr::from_ptr(p) }.to_bytes())
            .collect();
        assert_eq!(args, [&b"./prog"[..], b"ab", b"c"]);
        assert!(table[3].is_null());
        assert_eq!(&a.block[..12], b"title-over-\0");
        // SAFETY: `copies` came from `Box::into_raw` in `copy_and_repoint`,
        // and no pointer into it is used after this.
        drop(unsafe { Box::from_raw(copies) });
    }

    #[test]
    fn stat_fields_after_comm() {
        // `comm` with a space and a `)`; fields 48 and 49 hold the span
        let mut line = b"1234 (a b) c) S".to_vec();
        for n in 4..=52 {
            line.extend_from_slice(format!(" {}", n * 10).as_bytes());
        }
        assert_eq!(stat_field(&line, 4), Some(40));
        assert_eq!(arg_span_of(&line), Some((480, 490)));
        assert_eq!(stat_field(&line, 9), Some(90));
        assert_eq!(arg_span_of(b"1 (x) S 1 2"), None);
        assert_eq!(code_span_of(&line), Some((260, 270)));
        assert_eq!(stat_field(b"no comm here", 3), None);
    }

    /// On this process: the kernel's span of the arguments is not empty;
    /// the crate's code is in this test executable; the thread count. This
    /// test binary was single-threaded when the constructor ran (libtest
    /// starts its threads in `main`), so the constructor kept the region
    /// and repointed the table here; while this test runs, libtest's
    /// threads exist, so `no_other_thread` is false.
    #[test]
    #[cfg_attr(miri, ignore)] // `/proc` is not readable under Miri
    fn this_process() {
        let (start, end) = kernel_arg_span().unwrap();
        assert!(start < end);
        let code = (this_process as *const ()).addr();
        assert_eq!(in_main_executable(code), Some(true));
        // the stack is not the program's code
        let local = 0u8;
        assert_eq!(
            in_main_executable(std::ptr::addr_of!(local).addr()),
            Some(false)
        );
        let first_task_is_this = no_other_thread();
        let threads = std::fs::read_dir("/proc/self/task").unwrap().count();
        assert_eq!(first_task_is_this, threads == 1);
        assert!(matches!(initial(), Setup::Arguments(..)));
    }

    #[test]
    fn kernel_release_numbers() {
        assert_eq!(major_minor_of(b"7.0.0-1019-nvidia"), Some((7, 0)));
        assert_eq!(major_minor_of(b"5.11.22"), Some((5, 11)));
        assert_eq!(major_minor_of(b"6.12-rc1"), Some((6, 12)));
        assert_eq!(major_minor_of(b"x.1"), None);
        assert_eq!(major_minor_of(b"6"), None);
    }
}
