//! `sched-cases ID [ARGS...]`, or a link named `ID` to it (for
//! `scripts/cases.py check --exe-dir`): the Rust port of
//! `tests/cases/{tasks,sync,refs}/ID.lean`, run through `lean_runtime::sched`
//! with the glue a translator writes.

mod cases;
mod glue;
mod lean;

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let name = std::path::Path::new(&argv[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let (id, args) = match cases::lookup(&name) {
        Some(_) => (name, &argv[1..]),
        None if argv.len() > 1 => (argv[1].clone(), &argv[2..]),
        None => {
            eprintln!("usage: sched-cases ID [ARGS...]");
            std::process::exit(2)
        }
    };
    let Some((init, main)) = cases::lookup(&id) else {
        eprintln!("sched-cases: no case {id}");
        std::process::exit(2)
    };
    glue::run(init, main, args)
}
