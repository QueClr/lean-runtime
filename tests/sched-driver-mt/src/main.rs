//! `sched-cases-mt ID [ARGS...]`: the Rust port of
//! `tests/cases/{tasks,sync,refs,taskio,net}/ID.lean` in threads mode,
//! Lean's task manager on real threads (`lean_runtime::sched`, which a
//! `threads` build re-exports from `sched::mt`), and `net` on `sched::uv`'s
//! loop thread, with the glue a translator writes.
//!
//! The ports are the single-thread driver's (`tests/sched-driver/src/cases.rs`
//! and `netcases.rs`, with Lean's IO definitions in `lio.rs`, the glue of
//! `net` in `lnet.rs` and the scheduler-independent glue in
//! `glue_common.rs`), included by path. This package gives what differs in
//! threads mode: the values (`lean.rs`: `Arc`, a `OnceLock` slot, the 4.35
//! rule of `sched::Ref`) and the `Glue` with the program's entry
//! (`glue.rs`).
//!
//! `sched-cases-mt --list` prints the ids it runs, one per line;
//! `sched-cases-mt --single-thread-only` prints the cases it does not run,
//! each with the reason (`cases::SINGLE_THREAD_ONLY`).

#[path = "../../sched-driver/src/cases.rs"]
mod cases;
mod glue;
#[path = "../../sched-driver/src/glue_common.rs"]
mod glue_common;
mod lean;
// Lean's IO definitions; some serve only the single-thread driver's cases.
#[allow(dead_code)]
#[path = "../../sched-driver/src/lio.rs"]
mod lio;
#[path = "../../sched-driver/src/lnet.rs"]
mod lnet;
#[path = "../../sched-driver/src/netcases.rs"]
mod netcases;

/// The reason case `id` runs over the single-thread scheduler only, if it
/// does.
fn single_thread_only(id: &str) -> Option<&'static str> {
    cases::SINGLE_THREAD_ONLY
        .iter()
        .find(|(n, _)| *n == id)
        .map(|&(_, why)| why)
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let Some(id) = argv.get(1) else {
        eprintln!("usage: sched-cases-mt ID [ARGS...] | --list | --single-thread-only");
        std::process::exit(2)
    };
    match id.as_str() {
        "--list" => {
            for (id, _) in cases::CASES.iter().chain(netcases::CASES) {
                if single_thread_only(id).is_none() {
                    println!("{id}");
                }
            }
            return;
        }
        "--single-thread-only" => {
            for (id, why) in cases::SINGLE_THREAD_ONLY {
                println!("{id}\t{why}");
            }
            return;
        }
        _ => {}
    }
    if let Some(why) = single_thread_only(id) {
        eprintln!("sched-cases-mt: {id} runs over the single-thread scheduler only: {why}");
        std::process::exit(2)
    }
    let Some((init, main)) = cases::lookup(id).or_else(|| netcases::lookup(id)) else {
        eprintln!("sched-cases-mt: no case {id}");
        std::process::exit(2)
    };
    glue::run(init, main, &argv[2..])
}
