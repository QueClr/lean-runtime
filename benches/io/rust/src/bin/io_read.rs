//! `Handle.read k` of 1 to 64 bytes (chosen by the LCG) from a 1 MiB file, rewinding at its end:
//! the copy out of glibc's buffer, one `read` per buffer, and a new byte array of `k` bytes per call
//! (Lean allocates it before `fread`; a translator allocates its own). The native twin is
//! `benches/io/native/Bench/IoRead.lean`.

use lean_runtime::io::handle::check_read_size;
use lean_runtime::io::{FsMode, Handle};
use lean_runtime_io_bench::*;

struct Input {
    _file: TempFile,
    h: Handle,
}

fn build() -> Input {
    let file = TempFile::new("read", &random_bytes(1 << 20));
    let h = Handle::open(file.0.as_bytes(), FsMode::Read).unwrap();
    Input { _file: file, h }
}

#[inline(never)]
fn kernel(inp: &Input, n: u64) -> u64 {
    let (mut x, mut acc) = (SEED, 0u64);
    for _ in 0..n {
        x = step(x);
        let k = 1 + (x >> 58) as usize;
        check_read_size(k).unwrap();
        let mut b = vec![0u8; k];
        let got = inp.h.read(&mut b).unwrap();
        b.truncate(got);
        if got < k {
            inp.h.rewind().unwrap();
        }
        let first = b.first().copied().unwrap_or(0) as u64;
        acc = mix(acc, got as u64 ^ (first << 8));
    }
    acc
}

fn main() {
    run(build, kernel)
}
