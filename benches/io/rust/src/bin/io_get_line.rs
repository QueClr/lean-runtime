//! `Handle.getLine` over a 64 KiB file of lines of 1 to 79 bytes, rewinding at its end: the scan
//! and copy out of glibc's buffer, one `read` per buffer, and a new string per line, checked as
//! UTF-8 and its characters counted, as Lean's `mk_string` does (the line is appended straight into
//! the translator's own object, a `Vec` here). The native twin is
//! `benches/io/native/Bench/IoGetLine.lean`.

use lean_runtime::io::{FsMode, Handle};
use lean_runtime_io_bench::*;

struct Input {
    _file: TempFile,
    h: Handle,
}

fn build() -> Input {
    let file = TempFile::new("lines", &random_lines(1 << 16));
    let h = Handle::open(file.0.as_bytes(), FsMode::Read).unwrap();
    Input { _file: file, h }
}

#[inline(never)]
fn kernel(inp: &Input, n: u64) -> u64 {
    let (mut x, mut acc) = (SEED, 0u64);
    for _ in 0..n {
        x = step(x);
        let mut line = Vec::new();
        inp.h.get_line(&mut line).unwrap();
        let chars = std::str::from_utf8(&line).map_or(0, |s| s.chars().count());
        if line.is_empty() {
            inp.h.rewind().unwrap();
        }
        acc = mix(acc, chars as u64);
    }
    acc
}

fn main() {
    run(build, kernel)
}
