//! `sched-cases ID [ARGS...]`, or a link named `ID` to it (for
//! `scripts/cases.py check --exe-dir`): the Rust port of
//! `tests/cases/{tasks,sync,refs,taskio,uvloop,net}/ID.lean` (and of the io
//! and process cases with tasks), run through `lean_runtime::sched` with the
//! glue a translator writes. `cases.rs` and `netcases.rs` (with its glue,
//! `lnet.rs`) hold the ports the threads-mode driver
//! (`tests/sched-driver-mt`) shares; `cases_st.rs` the ones of the
//! single-thread scheduler only.

mod cases;
mod cases_st;
mod glue;
mod glue_common;
mod lean;
mod lio;
mod lnet;
mod netcases;
mod review;
mod wait1;

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let name = std::path::Path::new(&argv[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let lookup = |id: &str| cases::lookup(id).or_else(|| cases_st::lookup(id));
    let (id, args) = match lookup(&name) {
        Some(_) => (name, &argv[1..]),
        None if argv.len() > 1 => (argv[1].clone(), &argv[2..]),
        None => {
            eprintln!("usage: sched-cases ID [ARGS...]");
            std::process::exit(2)
        }
    };
    let Some((init, main)) = lookup(&id) else {
        eprintln!("sched-cases: no case {id}");
        std::process::exit(2)
    };
    // A program installed before the glue's opt-in (review SO-1 of AR-11).
    if id == "so_prev_resethand" {
        review::install_prev_resethand();
    }
    glue::run(init, main, args)
}
