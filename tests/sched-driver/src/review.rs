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
/// that would wait is set aside, and done when the scope ends. Without the
/// scope (argument `plain`), the context suspends in the drop. Either way
/// the child (`wc -c`) gets every byte.
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
        // the set-aside flush and close, waiting cooperatively now
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
