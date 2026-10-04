//! `Handle.putStr` of short strings (1 to 64 bytes, chosen by the LCG) on a handle over
//! `/dev/null`: the copy into glibc's buffer, and one `write` per full buffer. The native twin is
//! `benches/io/native/Bench/IoPutStr.lean`.

use lean_runtime::io::{FsMode, Handle};
use lean_runtime_io_bench::*;

struct Input {
    h: Handle,
    strs: Vec<String>,
}

fn build() -> Input {
    let h = Handle::open(b"/dev/null", FsMode::Write).unwrap();
    let mut x = INPUT_SEED;
    let strs = (0..64)
        .map(|k| {
            (0..=k)
                .map(|_| {
                    x = step(x);
                    (b'a' + ((x >> 59) as u8 % 26)) as char
                })
                .collect()
        })
        .collect();
    Input { h, strs }
}

#[inline(never)]
fn kernel(inp: &Input, n: u64) -> u64 {
    let (mut x, mut acc) = (SEED, 0u64);
    for _ in 0..n {
        x = step(x);
        let s = &inp.strs[(x >> 58) as usize];
        if inp.h.put_str(s.as_bytes()).is_err() {
            acc = !acc;
        }
        acc = mix(acc, s.len() as u64);
    }
    acc
}

fn main() {
    run(build, kernel)
}
