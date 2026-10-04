//! The harness of lean-runtime's io micro-benchmarks (`benches/io/README.md`). Each binary in
//! `src/bin/` times one hot path of `lean_runtime::io` as a translator's code calls it, with the
//! glue a translator pays (a new byte array or string object per result); its native twin of the
//! same name in `benches/io/native/` makes the same calls through Lean's API, on the same inputs,
//! and prints the same checksum.
//!
//! What both sides share, value for value with `benches/io/native/Harness.lean`: the LCG `step`
//! and its seeds, the inputs (built before the first clock read), the checksum `mix`.
//!
//! A binary takes one argument, the number of iterations N. It prints the checksum on stdout line 1
//! and `kernel_ns <ns>` on line 2: monotonic time read just before the kernel and just after it,
//! the input built before and the result consumed after, through `black_box`. The input files live
//! in the system's temporary directory, named after the process, and are removed at the end. The
//! allocator is mimalloc, as in Lean's runtime and both translators.

#![forbid(unsafe_code)]

use std::hint::black_box;
use std::time::Instant;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The LCG both sides draw from: `x' = x * A + C` modulo 2^64.
#[inline(always)]
pub fn step(x: u64) -> u64 {
    x.wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
}

/// The LCG's first state in the timed loop.
pub const SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// The LCG's first state when the inputs are built.
pub const INPUT_SEED: u64 = 0x2545_F491_4F6C_DD1D;

/// The checksum step: rotate and xor.
#[inline(always)]
pub fn mix(acc: u64, r: u64) -> u64 {
    acc.rotate_left(7) ^ r
}

/// `size` bytes drawn from the LCG (the byte is the state's top 8 bits).
pub fn random_bytes(size: usize) -> Vec<u8> {
    let mut x = INPUT_SEED;
    (0..size)
        .map(|_| {
            x = step(x);
            (x >> 56) as u8
        })
        .collect()
}

/// Lines of 1 to 79 bytes (the newline included), ASCII letters, `size` bytes in all at least.
pub fn random_lines(size: usize) -> Vec<u8> {
    let mut x = INPUT_SEED;
    let mut v = Vec::with_capacity(size + 81);
    while v.len() < size {
        x = step(x);
        let len = (x >> 58) as usize + (x >> 52 & 15) as usize; // 0 to 78
        for _ in 0..len {
            x = step(x);
            v.push(b'a' + ((x >> 59) as u8 % 26));
        }
        v.push(b'\n');
    }
    v
}

/// A file in the temporary directory, named after the process, removed on drop.
pub struct TempFile(pub String);

impl TempFile {
    pub fn new(tag: &str, contents: &[u8]) -> TempFile {
        let p = format!("/tmp/lean-runtime-bench-{tag}-{}", std::process::id());
        std::fs::write(&p, contents).unwrap();
        TempFile(p)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs one benchmark (see the module comment).
pub fn run<I>(build: impl FnOnce() -> I, kernel: impl FnOnce(&I, u64) -> u64) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let n: u64 = match args.as_slice() {
        [n] => n.parse().expect("N"),
        _ => {
            eprintln!("usage: <benchmark> N");
            std::process::exit(2)
        }
    };
    let input = black_box(build());
    let n = black_box(n);
    let t0 = Instant::now();
    let out = black_box(kernel(&input, n));
    let ns = t0.elapsed().as_nanos();
    println!("{out}");
    println!("kernel_ns {ns}");
}
