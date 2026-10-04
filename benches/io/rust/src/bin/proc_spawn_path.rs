//! `IO.Process.spawn` of `true`, found through `PATH` as `execvp` finds it (`/usr/local/bin`,
//! then `/usr/bin`, then `/bin`, set by the binary itself so both sides search the same
//! entries), every standard stream `null`, then `Child.wait`. The native twin is
//! `benches/io/native/Bench/ProcSpawnPath.lean`.

use lean_runtime::io::process::{self, SpawnArgs, Stdio, StdioConfig};
use lean_runtime_io_bench::*;

const CFG: StdioConfig = StdioConfig {
    stdin: Stdio::Null,
    stdout: Stdio::Null,
    stderr: Stdio::Null,
};

struct Input {
    cmd: Vec<u8>,
    path: Vec<u8>,
}

#[inline(never)]
fn kernel(inp: &Input, n: u64) -> u64 {
    let mut acc = 0u64;
    for i in 0..n {
        let env = [(&b"PATH"[..], Some(inp.path.as_slice()))];
        let args = SpawnArgs {
            cmd: &inp.cmd,
            args: &[],
            cwd: None,
            env: &env,
            inherit_env: true,
            setsid: false,
        };
        // an error of either call: 4294967294, as the native twin's `tryCatch`
        let code = process::spawn(CFG, &args)
            .and_then(|c| c.process.wait())
            .unwrap_or(u32::MAX - 1);
        acc = mix(acc, u64::from(code) ^ i);
    }
    acc
}

fn main() {
    run(
        || Input {
            cmd: b"true".to_vec(),
            path: b"/usr/local/bin:/usr/bin:/bin".to_vec(),
        },
        kernel,
    )
}
