//! Regression programs from the reviews of sched-io (RSIO, rounds 1 and
//! 2; round 2's are the reviewer's probes), not Lean cases: each checks one guarantee of the crate, and
//! `tests/cases.rs` checks its output.

use crate::glue::{eprintln, println};
use crate::lean::*;
use crate::lio;
use lean_runtime::io::error::errno;
use lean_runtime::io::process::{Stdio, StdioConfig};
use lean_runtime::io::Handle;
use lean_runtime::sched::{self, Interest};
use std::cell::Cell;
use std::os::fd::AsFd;
use std::rc::Rc;

/// RSIO-01: a task holds stdout's guard (`Handle::file()`) across
/// `wait_fd`; `main` then prints. Every suspension records the task's
/// stream locks, so `main` waits for stdout instead of deadlocking.
pub fn rsio_poll_fds_with_stream_lock(_: &[String]) -> u32 {
    let (r, mut w) = std::io::pipe().unwrap();
    std::thread::spawn(move || {
        use std::io::Write;
        std::thread::sleep(std::time::Duration::from_millis(200));
        w.write_all(b"x").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2000));
    });
    let t = as_task(
        move || {
            let out = Handle::stdout();
            let g = out.file();
            let _ = sched::wait_fd(r.as_fd(), Interest::READ);
            drop(g);
            eprintln("task: waited holding stdout");
        },
        PRIO_DEDICATED,
    );
    sleep(50);
    eprintln("main: printing");
    println("main: printed");
    t.get();
    eprintln("main: done");
    0
}

/// This process's CPU time so far, in clock ticks (`/proc/self/stat`'s
/// `utime` plus `stime`).
fn cpu_ticks() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap();
    let rest = &s[s.rfind(')').unwrap() + 2..];
    let f: Vec<&str> = rest.split(' ').collect();
    // fields 14 and 15 of the line, 12 and 13 after the name
    f[11].parse::<u64>().unwrap() + f[12].parse::<u64>().unwrap()
}

/// RSIO-02: a watch whose callback blocks (a 300 ms sleep) while its
/// descriptor stays readable. The watch is out of epoll's set until the
/// call returns: the hub does not spin meanwhile, and no second call runs
/// on the drained descriptor.
pub fn rsio_watch_spin(_: &[String]) -> u32 {
    use std::io::{Read, Write};
    let (r, mut w) = std::io::pipe().unwrap();
    let r = Rc::new(r);
    let p: Promise<u32> = Promise::new();
    let res = p.result_opt();
    let p = Rc::new(p);
    let calls = Rc::new(Cell::new(0u32));
    let cpu = Rc::new(Cell::new(0u64));
    let (p2, c2, cpu2, r2) = (p.clone(), calls.clone(), cpu.clone(), r.clone());
    let id = sched::watch(
        r.clone(),
        Interest::READ,
        Rc::new(move |_ready| {
            c2.set(c2.get() + 1);
            if c2.get() > 1 {
                return;
            }
            let t0 = cpu_ticks();
            sched::sleep_ms(300);
            cpu2.set(cpu_ticks() - t0);
            let mut b = [0u8; 8];
            let _ = (&*r2).read(&mut b);
            p2.resolve(1);
        }),
    )
    .unwrap();
    w.write_all(b"x").unwrap();
    let _ = res.get();
    // let a second call, if one were queued, run
    sleep(50);
    sched::unwatch(id);
    println(&format!("callback calls: {}", calls.get()));
    // 300 ms of spinning is about 30 ticks (USER_HZ 100); an idle wait none
    println(&format!(
        "under 100 ms of CPU while the callback slept: {}",
        cpu.get() < 10
    ));
    0
}

/// RSIO-03 and RSIO-09: dropping a handle's last reference flushes it, and
/// the flush may wait for a full pipe. In a no-suspend scope (the glue's
/// free and drop paths) no other context runs during the drop: the flush
/// that would wait is set aside, and done at the context's next scheduling
/// point (AR-8: its next line, not the scope's end). Without the scope
/// (argument `plain`), the context suspends in the drop. Either way the
/// child (`wc -c`) gets every byte.
pub fn rsio_drop_no_suspend(args: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let ticks = Rc::new(Cell::new(0u32));
    let t2 = ticks.clone();
    let ticker = as_task(
        move || {
            for _ in 0..6 {
                sleep(40);
                t2.set(t2.get() + 1);
            }
        },
        PRIO_DEDICATED,
    );
    // the ticker starts on a context of its own
    sleep(1);
    let stdin = child.stdin.expect("piped");
    // 64 KiB fill the pipe; the last 100 bytes stay buffered until the drop
    let data = vec![b'x'; 65536 + 100];
    stdin.put_str(&data).unwrap();
    let before = ticks.get();
    let during = if args.first().map(String::as_str) == Some("plain") {
        drop(stdin);
        ticks.get() - before
    } else {
        let scope = sched::no_suspend();
        drop(stdin);
        let d = ticks.get() - before;
        // the leave: no flush, no suspension (AR-8); the set-aside flush and
        // close wait cooperatively at the next line's effect point
        drop(scope);
        d
    };
    println(&format!("ticks during the drop: {}", during.min(1)));
    ticker.get();
    let _ = child.process.wait();
    0
}

// ---------------------------------------------------------------------------
// Round 2 (the reviewer's probes, review-sched-io/repro/rsio2.rs)

/// RSIO-09: a child writes 200000 bytes to stdout first, then
/// reads its stdin. A task reads the child's stdout. Main writes 65536+100
/// bytes to the child's stdin (the 64 KiB fill the pipe; 100 stay buffered)
/// and drops the handle without a flush, in a no-suspend scope as a
/// translator's drop path would (arg `plain`: without the scope).
pub fn rsio_ns_drop_deadlock(args: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "head -c 200000 /dev/zero; cat >/dev/null"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let out = child.stdout.clone().expect("piped");
    let reader = as_task(
        move || lio::read_bin_to_end(&out).map(|b| b.len()),
        PRIO_DEDICATED,
    );
    let stdin = child.stdin.expect("piped");
    let data = vec![b'A'; 65536 + 100];
    stdin.put_str(&data).unwrap();
    eprintln("main: wrote, dropping stdin");
    if args.first().map(String::as_str) == Some("plain") {
        drop(stdin);
    } else {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    eprintln("main: dropped");
    match reader.get() {
        Ok(n) => println(&format!("read {n}")),
        Err(e) => println(&format!("err {e:?}")),
    }
    println(&format!("exit {:?}", child.process.wait().ok()));
    0
}

/// RSIO-10: the no-suspend depth is per thread, and the
/// scheduler's own waits still switch inside a scope. Main drops a promise
/// in a no-suspend scope (a translator's drop path); its sync dependent (an
/// `IO.mapTask (sync := true)`) runs there, sleeps 200 ms, then writes a
/// line to `cat`. A task that starts reading `cat`'s output during that
/// sleep must not inherit the scope: its read waits cooperatively, and
/// `cat` gets the line.
pub fn rsio_ns_leak(args: &[String]) -> u32 {
    let child = lio::spawn(
        "cat",
        &[],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let out = child.stdout.clone().expect("piped");
    let reader = as_task(
        move || {
            sleep(50);
            eprintln(&format!(
                "reader: io_cooperative = {}",
                sched::io_cooperative()
            ));
            lio::get_line(&out)
        },
        PRIO_DEDICATED,
    );
    let stdin = child.stdin.clone().expect("piped");
    let p: Promise<u32> = Promise::new();
    let dep = map_task(
        move |_| {
            sleep(200);
            let _ = stdin.put_str(b"hi\n");
            let _ = stdin.flush();
            0u32
        },
        p.result_opt(),
        PRIO_DEFAULT,
        true,
        true,
    );
    if args.first().map(String::as_str) == Some("plain") {
        drop(p);
    } else {
        let _scope = sched::no_suspend();
        drop(p);
    }
    let _ = dep.get();
    match reader.get() {
        Ok(l) => println(&format!("reader got {}", lio::quote(&l))),
        Err(e) => println(&format!("err {e:?}")),
    }
    0
}

/// RSIO-09, a commoner shape: taskio/task_reads_main_writes without the
/// final `stdin.flush` (the handle's drop flushes the rest), the drop in a
/// no-suspend scope (arg `plain`: without it).
pub fn rsio_ns_cat(args: &[String]) -> u32 {
    let n = to_nat(&args[0]);
    let len = to_nat(&args[1]);
    let child = lio::spawn(
        "cat",
        &[],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let stdin = child.stdin.expect("piped");
    let out = child.stdout.expect("piped");
    let reader = as_task(
        move || lio::read_to_end(&out).map(|s| s.len()),
        PRIO_DEFAULT,
    );
    let mut line = "x".repeat(len as usize);
    line.push('\n');
    for _ in 0..n {
        stdin.put_str(line.as_bytes()).unwrap();
    }
    if args.get(2).map(String::as_str) == Some("plain") {
        drop(stdin);
    } else {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    println("main: wrote everything");
    println(&format!("task: read {:?}", reader.get().ok()));
    println(&format!("cat exited {:?}", child.process.wait().ok()));
    0
}

/// RSIO-10, the panic: task A sleeps inside a no-suspend scope; task C holds
/// stdout's guard across a wait; main, in no scope, prints meanwhile.
pub fn rsio_ns_leak_panic(_: &[String]) -> u32 {
    use lean_runtime::io::Handle;
    use std::os::fd::AsFd;
    let (r, mut w) = std::io::pipe().unwrap();
    std::thread::spawn(move || {
        use std::io::Write;
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = w.write_all(b"x");
        std::thread::sleep(std::time::Duration::from_millis(2000));
    });
    let c = as_task(
        move || {
            let out = Handle::stdout();
            let g = out.file();
            let _ = sched::wait_fd(r.as_fd(), sched::Interest::READ);
            drop(g);
        },
        PRIO_DEDICATED,
    );
    let a = as_task(
        || {
            let _scope = sched::no_suspend();
            sleep(300);
        },
        PRIO_DEDICATED,
    );
    sleep(50);
    eprintln("main: printing (no scope here)");
    println("main: printed");
    a.get();
    c.get();
    eprintln("main: done");
    0
}

// ---------------------------------------------------------------------------
// Round 3 (the reviewer's probe, review-sched-io/repro/rsio-round3-probes.patch)

/// RSIO-12 and RSIO-13 (round 3): a child that reads its stdin only after
/// 0.4 s and counts it. Main fills
/// the pipe to 65440 bytes (16 pages, the last one 96 bytes short), puts
/// 500 more bytes (buffered) and drops the handle in a no-suspend scope
/// (`plain`: without it). Arg 2 `full`: fill to exactly 65536 instead. The
/// child gets every byte, and the modelled errno is as before the drop.
pub fn rsio_ns_partial(args: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.4; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    // a task, so that the program has tasks (coop_possible)
    let t = as_task(|| sleep(5), PRIO_DEDICATED);
    t.get();
    let stdin = child.stdin.expect("piped");
    let fill = if args.get(1).map(String::as_str) == Some("full") {
        65536
    } else {
        65440
    };
    stdin.put_str(&vec![b'a'; fill]).unwrap();
    stdin.flush().unwrap();
    stdin.put_str(&vec![b'b'; 500]).unwrap();
    let before = errno();
    if args.first().map(String::as_str) == Some("plain") {
        drop(stdin);
    } else {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    let after = errno();
    let _ = child.process.wait();
    println(&format!(
        "expected {} bytes; errno before {before} after {after}",
        fill + 500
    ));
    0
}

// ---------------------------------------------------------------------------
// AR-8 (lean-runtime fixes-1): leaving the scope never suspends

/// AR-8: a translator's drop walk that ends inside a panic's unwinding (a
/// `Drop` of the unwound frame), with a stream it sets aside (its pipe to
/// `wc -c` full, 100 bytes still buffered). The leave neither flushes nor
/// suspends: the ticker task does not run during it, with the panic in
/// flight. The child still gets every byte: at `main`'s next scheduling
/// point (its next line), or, with argument `exit`, when `main` has
/// returned (`sched::finish`, or the exit's flush). The walk's report goes
/// straight to descriptor 2 (Rust's `stderr`), which is no scheduling point.
pub fn rsio_ns_unwind(args: &[String]) -> u32 {
    use std::io::Write;
    let at_exit = args.first().map(String::as_str) == Some("exit");
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let ticks = Rc::new(Cell::new(0u32));
    let t2 = ticks.clone();
    let ticker = as_task(
        move || {
            for _ in 0..6 {
                sleep(40);
                t2.set(t2.get() + 1);
            }
        },
        PRIO_DEDICATED,
    );
    // the ticker starts on a context of its own
    sleep(1);
    let stdin = child.stdin.expect("piped");
    stdin.put_str(&vec![b'x'; 65536 + 100]).unwrap();

    /// The drop walk, run by the unwinding.
    struct Walk {
        h: Option<Handle>,
        ticks: Rc<Cell<u32>>,
    }
    impl Drop for Walk {
        fn drop(&mut self) {
            let before = self.ticks.get();
            sched::enter_no_suspend();
            drop(self.h.take());
            sched::leave_no_suspend();
            let _ = writeln!(
                std::io::stderr(),
                "drop walk: panicking {}, ticks during the leave {}",
                std::thread::panicking(),
                self.ticks.get() - before
            );
        }
    }
    let quiet = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _walk = Walk {
            h: Some(stdin),
            ticks: ticks.clone(),
        };
        panic!("unwinding through the drop walk");
    }));
    std::panic::set_hook(quiet);
    assert!(r.is_err());
    if at_exit {
        return 0;
    }
    // a scheduling point of `main`: the set-aside stream closes here
    println("main: after the unwind");
    ticker.get();
    let _ = child.process.wait();
    0
}

/// AR-8, leanrs's review of fixes-1: `IO.Process.forceExit` (`_Exit`) right
/// after a drop in a no-suspend scope set a stream aside (its pipe to
/// `wc -c` full, 100 bytes still buffered), with no scheduling point in
/// between. Natively the drop's `fclose` wrote the bytes before any
/// `_Exit`: `force_exit` closes the set-aside streams first, so the child
/// gets every byte (65636), and flushes nothing else (`main`'s buffered
/// line is lost, as `_Exit` loses it).
pub fn rsio_ns_force_exit(_: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    // a task, so that the program has tasks (coop_possible)
    let t = as_task(|| sleep(5), PRIO_DEDICATED);
    t.get();
    let stdin = child.stdin.expect("piped");
    stdin.put_str(&vec![b'x'; 65536 + 100]).unwrap();
    println("main: buffered, lost at _Exit");
    {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    lean_runtime::io::exit::force_exit(0)
}

// ---------------------------------------------------------------------------
// Review fixes-1 probes (RFX1, the reviewer's): AR-8's hand-off must let these end

/// RFX1 probe: a child writes 300000 bytes to stdout, then reads stdin to
/// its end. Task B reads the child's stdout (a shared handle). Main writes
/// 65536+100 bytes to the child's stdin and drops the handle in a no-suspend
/// scope (set aside: the pipe is full, the child does not read it yet), then
/// reads the child's stdout itself. Natively the drop's `fclose` waits until
/// the child reads stdin (B drains its stdout meanwhile), and everything ends.
/// Modes: "" (B reads without pause: main waits for the handle's lock in
/// `block_sync`), "sleepy" (B pauses 1 ms between reads: main takes the
/// lock and settles inside `before_read`), "plain" (no scope).
/// The progress report goes straight to descriptor 2 (no scheduling point).
pub fn rfx1_shared_read(args: &[String]) -> u32 {
    use std::io::Write;
    let mode = args.first().cloned().unwrap_or_default();
    let sleepy = mode == "sleepy" || mode == "sleepy-plain";
    let plain = mode == "plain" || mode == "sleepy-plain";
    let child = lio::spawn(
        "sh",
        &["-c", "head -c 300000 /dev/zero; cat >/dev/null"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let out = child.stdout.clone().expect("piped");
    let out2 = out.clone();
    let reader = as_task(
        move || {
            let mut n = 0usize;
            loop {
                match lio::read(&out2, 1024) {
                    Ok(b) if b.is_empty() => break,
                    Ok(b) => {
                        n += b.len();
                        if sleepy {
                            sleep(1);
                        }
                    }
                    Err(_) => break,
                }
            }
            n
        },
        PRIO_DEDICATED,
    );
    sleep(1);
    let stdin = child.stdin.expect("piped");
    stdin.put_str(&vec![b'A'; 65536 + 100]).unwrap();
    if plain {
        drop(stdin);
    } else {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    let _ = writeln!(std::io::stderr(), "main: dropped, reading");
    let got = lio::read(&out, if sleepy { 8192 } else { 1 }).map(|b| b.len() <= 8192);
    let _ = writeln!(std::io::stderr(), "main: read {got:?}");
    let n = reader.get();
    println(&format!("main read {got:?}; reader read some: {}", n > 0));
    println(&format!("exit {:?}", child.process.wait().ok()));
    0
}

/// RFX1 probe: a program with a task; main writes 65536+100 bytes to a
/// child (`sleep 0.3; wc -c`) and drops the handle in a no-suspend scope (set
/// aside: the pipe is full), then polls `Child.tryWait` until the child has
/// exited, with no other call in the loop. Natively the drop's `fclose`
/// waits until `wc` reads, closes, and the loop ends.
pub fn rfx1_trywait(_: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let t = as_task(|| sleep(5), PRIO_DEDICATED);
    t.get();
    let stdin = child.stdin.expect("piped");
    stdin.put_str(&vec![b'a'; 65536 + 100]).unwrap();
    {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    let mut spins = 0u64;
    let code = loop {
        if let Some(c) = child.process.try_wait().unwrap() {
            break c;
        }
        spins += 1;
    };
    println(&format!("child exited {code} (spun: {})", spins > 0));
    0
}

/// LB-29's scope, the writers of dropped streams at exit: `rsio_exit_join`
/// in `tests/cases.rs`. The child writes 300000 bytes to its stdout, which
/// a task drains, then counts its stdin to stderr. `main` writes 65636
/// bytes to its stdin, drops the handle in a no-suspend scope (handed to a
/// writer thread: the pipe is full), then exits (`exit`: `IO.Process.exit
/// 0`; `force`: `forceExit 0`).
pub fn rsio_exit_join(args: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "head -c 300000 /dev/zero; wc -c >&2"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let out = child.stdout.clone().expect("piped");
    let _reader = as_task(
        move || lio::read_bin_to_end(&out).map(|b| b.len()),
        PRIO_DEDICATED,
    );
    let stdin = child.stdin.expect("piped");
    stdin.put_str(&vec![b'A'; 65536 + 100]).unwrap();
    {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    if args.first().map(String::as_str) == Some("force") {
        lean_runtime::io::exit::force_exit(0)
    }
    println("main: exiting");
    crate::glue::process_exit(0)
}

// ---------------------------------------------------------------------------
// Review fixes-1 round 2 probes (RFX1, the reviewer's)

/// Round-2 probe: a child writes 300000 bytes to stdout, sleeps 0.5 s,
/// writes 300000 more, then reads stdin to its end. Task B reads the
/// child's stdout to its end. Main writes 65536+100 bytes to the child's
/// stdin, drops the handle in a no-suspend scope (handed off: the pipe is
/// full), then ends: "exit" (IO.Process.exit 0), "force" (forceExit 0) or
/// "return" (main returns). Natively the drop's `fclose` waits until the
/// child reads stdin (B drains its stdout meanwhile), then the exit.
pub fn rfx2_exit_handoff(args: &[String]) -> u32 {
    use std::io::Write;
    let mode = args.first().cloned().unwrap_or_default();
    let child = lio::spawn(
        "sh",
        &[
            "-c",
            "head -c 300000 /dev/zero; sleep 0.5; head -c 300000 /dev/zero; cat >/dev/null",
        ],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Piped,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let out = child.stdout.clone().expect("piped");
    let _reader = as_task(
        move || lio::read_bin_to_end(&out).map(|b| b.len()).unwrap_or(0),
        PRIO_DEDICATED,
    );
    sleep(1);
    let stdin = child.stdin.expect("piped");
    stdin.put_str(&vec![b'A'; 65536 + 100]).unwrap();
    {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    let _ = writeln!(std::io::stderr(), "main: dropped, ending ({mode})");
    match mode.as_str() {
        "exit" => crate::glue::process_exit(0),
        "force" => lean_runtime::io::exit::force_exit(0),
        _ => 0,
    }
}

/// Round-2 probe: a task drops the stdin of `sleep 30` (the reviewer's
/// `sleep 3`, longer here so that waiting for it fails the test), 65536+100 bytes
/// written (handed off: `sleep` never reads), then main calls
/// IO.Process.exit 3 after 300 ms. Natively the task's thread is blocked in
/// `fclose`, which unlinked the stream from glibc's list first, so the exit
/// does not wait for it: exit 3 at once.
pub fn rfx2_exit_unrelated_handoff(_: &[String]) -> u32 {
    use std::io::Write;
    // `sleep` holds none of the runner's pipes, which would keep its reads
    // open for 30 s
    let child = lio::spawn(
        "sleep",
        &["30"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Null,
            stderr: Stdio::Null,
        },
    )
    .unwrap();
    let stdin = child.stdin.clone().expect("piped");
    drop(child);
    let _t = as_task(
        move || {
            stdin.put_str(&vec![b'a'; 65536 + 100]).unwrap();
            {
                let _scope = sched::no_suspend();
                drop(stdin);
            }
            // not ended when `main` exits: its writer is still running
            sleep(5000);
        },
        PRIO_DEDICATED,
    );
    sleep(300);
    let _ = writeln!(std::io::stderr(), "main: exiting");
    crate::glue::process_exit(3)
}

/// Round-2 probe (LB-29's narrowed rule): task B prints a 200000-byte line
/// to stdout, a pipe whose reader starts after 1 s, so B is suspended in
/// the write holding stdout; main then calls IO.Process.exit 0. Natively
/// the exit waits for B's write (the reader drains it), then flushes: the
/// reader counts 200001 bytes.
pub fn rfx2_exit_writer_held(_: &[String]) -> u32 {
    let _b = as_task(
        || {
            println(&"y".repeat(200000));
        },
        PRIO_DEDICATED,
    );
    sleep(50);
    crate::glue::process_exit(0)
}

/// RFX1-07, the causal case (`rfx2_causal_handoff` in `tests/cases.rs`).
pub fn rfx2_causal_handoff(_: &[String]) -> u32 {
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let stdin = child.stdin.clone().expect("piped");
    drop(child);
    let t = as_task(
        move || {
            stdin.put_str(&vec![b'a'; 65536 + 100]).unwrap();
            let _scope = sched::no_suspend();
            drop(stdin);
        },
        PRIO_DEDICATED,
    );
    // the task starts on a context of its own (not run by `get` on `main`'s)
    sleep(1);
    t.get();
    crate::glue::process_exit(0)
}

// ---------------------------------------------------------------------------
// Review fixes-1 round 3 probes (RFX1, the reviewer's)

/// Round-3 probe: as `rfx2_causal_handoff`, but the task tells `main`
/// through a promise (also `Std.Channel`'s mechanism) instead of its value,
/// and goes on (sleeps 2 s) before its job ends. Main waits for the promise,
/// then "exit" (IO.Process.exit 0) or "return". Natively the task's drop
/// `fclose` ended before it resolved the promise: `wc` counts 65636.
pub fn rfx3_promise_handoff(args: &[String]) -> u32 {
    let mode = args.first().cloned().unwrap_or_default();
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let stdin = child.stdin.clone().expect("piped");
    drop(child);
    let p: Promise<u32> = Promise::new();
    let res = p.result_opt();
    let _t = as_task(
        move || {
            stdin.put_str(&vec![b'a'; 65536 + 100]).unwrap();
            {
                let _scope = sched::no_suspend();
                drop(stdin);
            }
            p.resolve(1);
            sleep(2000);
        },
        PRIO_DEDICATED,
    );
    let _ = res.get();
    if mode == "exit" {
        crate::glue::process_exit(0)
    }
    0
}

/// Round-3 probe: `IO.Process.exit` from an event-loop callback (a timer)
/// while task B is suspended writing a 200000-byte line to stdout, a pipe
/// whose reader starts after 1 s ("callback"); or two contexts calling
/// `IO.Process.exit` while B holds stdout ("two": a task exits 4 and main
/// exits 5). Natively every byte is written (the exit waits for B's lock).
pub fn rfx3_exit_contexts(args: &[String]) -> u32 {
    let mode = args.first().cloned().unwrap_or_default();
    let _b = as_task(
        || {
            println(&"y".repeat(200000));
        },
        PRIO_DEDICATED,
    );
    sleep(20);
    if mode == "callback" {
        let _id = sched::timer_start(
            std::time::Instant::now() + std::time::Duration::from_millis(30),
            Rc::new(|| crate::glue::process_exit(0)),
        );
        sleep(5000);
        return 9;
    }
    let _a = as_task(|| crate::glue::process_exit(4), PRIO_DEDICATED);
    sleep(10);
    crate::glue::process_exit(5)
}

// ---------------------------------------------------------------------------
// Review fixes-1 round 4 probes (RFX1, the reviewer's)

/// Round-4 probe: as `rfx3_promise_handoff`, but the task tells `main`
/// through the file system (`IO.FS.createDir "done"`, which takes no stream
/// lock) and goes on (sleeps 2 s). Main polls `System.FilePath.pathExists`
/// (with 1 ms sleeps), then IO.Process.exit 0. Natively the task's drop
/// `fclose` ended before the directory appeared: `wc` counts 65636.
pub fn rfx4_fs_signal(_: &[String]) -> u32 {
    use lean_runtime::io::fs as lfs;
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.3; wc -c"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let stdin = child.stdin.clone().expect("piped");
    drop(child);
    let _t = as_task(
        move || {
            stdin.put_str(&vec![b'a'; 65536 + 100]).unwrap();
            {
                let _scope = sched::no_suspend();
                drop(stdin);
            }
            let _ = lfs::create_dir(b"done");
            sleep(2000);
        },
        PRIO_DEDICATED,
    );
    while lfs::metadata(b"done").is_err() {
        sleep(1);
    }
    crate::glue::process_exit(0)
}

/// Round-4 probe: the descriptors open after a hand-off whose writer ends
/// before the dropping context's next point (the child reads after 50 ms;
/// main then spins 300 ms with no scheduling point). Natively the drop
/// closed the pipe: as many descriptors as before the spawn.
pub fn rfx4_fd_after(_: &[String]) -> u32 {
    let count = || std::fs::read_dir("/proc/self/fd").unwrap().count();
    let t = as_task(|| sleep(5), PRIO_DEDICATED);
    t.get();
    let before = count();
    let child = lio::spawn(
        "sh",
        &["-c", "sleep 0.05; cat >/dev/null"],
        StdioConfig {
            stdin: Stdio::Piped,
            stdout: Stdio::Inherit,
            stderr: Stdio::Inherit,
        },
    )
    .unwrap();
    let stdin = child.stdin.clone().expect("piped");
    stdin.put_str(&vec![b'a'; 65536 + 100]).unwrap();
    drop(child);
    {
        let _scope = sched::no_suspend();
        drop(stdin);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let after = count();
    sleep(10);
    let later = count();
    println(&format!(
        "descriptors: before {before}, after the writer {after}, after a sleep {later}"
    ));
    0
}
