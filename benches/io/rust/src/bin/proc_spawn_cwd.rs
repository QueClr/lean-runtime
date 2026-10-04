//! `IO.Process.spawn` of `/bin/true` in `/` with every standard stream `null`, then `Child.wait`: the
//! path of a spawn with a working directory (the spawner thread: a job sent and answered, `chdir`,
//! `posix_spawn`, then `waitpid`). The native twin is `benches/io/native/Bench/ProcSpawnCwd.lean`.

use lean_runtime::io::process::{self, SpawnArgs, Stdio, StdioConfig};
use lean_runtime_io_bench::*;

const CFG: StdioConfig = StdioConfig {
    stdin: Stdio::Null,
    stdout: Stdio::Null,
    stderr: Stdio::Null,
};

struct Input {
    cmd: Vec<u8>,
}

#[inline(never)]
fn kernel(inp: &Input, n: u64) -> u64 {
    let mut acc = 0u64;
    for i in 0..n {
        // the glue a translator pays: the arguments as views of its strings
        let args = SpawnArgs {
            cmd: &inp.cmd,
            args: &[],
            cwd: Some(b"/"),
            env: &[],
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
            cmd: b"/bin/true".to_vec(),
        },
        kernel,
    )
}
