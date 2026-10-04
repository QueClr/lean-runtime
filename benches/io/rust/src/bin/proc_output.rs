//! `IO.Process.output` of `/bin/echo <word>` (a word of 1 to 16 letters chosen by the LCG): two
//! pipes, `posix_spawn`, both pipes read to their end into new strings (validated as UTF-8, their
//! characters counted), `waitpid`. The native twin is `benches/io/native/Bench/ProcOutput.lean`.

use lean_runtime::io::process::{self, SpawnArgs};
use lean_runtime_io_bench::*;

struct Input {
    cmd: Vec<u8>,
    words: Vec<Vec<u8>>,
}

fn build() -> Input {
    let mut x = INPUT_SEED;
    let words = (0..16)
        .map(|k| {
            (0..=k)
                .map(|_| {
                    x = step(x);
                    b'a' + ((x >> 59) as u8 % 26)
                })
                .collect()
        })
        .collect();
    Input {
        cmd: b"/bin/echo".to_vec(),
        words,
    }
}

#[inline(never)]
fn kernel(inp: &Input, n: u64) -> u64 {
    let (mut x, mut acc) = (SEED, 0u64);
    for _ in 0..n {
        x = step(x);
        let w = inp.words[(x >> 60) as usize].as_slice();
        let args = SpawnArgs {
            cmd: &inp.cmd,
            args: &[w],
            cwd: None,
            env: &[],
            inherit_env: true,
            setsid: false,
        };
        // the translator's string objects for both outputs
        let (mut out, mut err) = (Vec::new(), Vec::new());
        match process::output(&args, None, &mut out, &mut err) {
            Ok(code) => {
                let o = String::from_utf8(out).unwrap_or_default();
                let e = String::from_utf8(err).unwrap_or_default();
                acc = mix(acc, u64::from(code));
                acc = mix(acc, o.chars().count() as u64);
                acc = mix(acc, e.chars().count() as u64);
            }
            Err(_) => acc = !acc,
        }
    }
    acc
}

fn main() {
    run(build, kernel)
}
