#!/usr/bin/env python3
"""Program cases (tests/cases/**/<id>.lean): build natively, record expected
output, and check a translator's executables against it.

  scripts/cases.py expect [--runs N] [CASE...]
      Build each case natively with the pinned Lean (LEAN_TOOLCHAIN, default
      ~/.elan/toolchains/leanprover--lean4---v4.34.0), run it N times
      (default 5), require identical results, and write <id>.out, <id>.err
      and <id>.code next to the case (a non-terminating case records the
      output seen before its timeout and the code "timeout").

  scripts/cases.py check --exe-dir DIR [CASE...]
      Run DIR/<id> (a translator's build of each case) and compare with the
      recorded expected files.

Each run starts in a new process group; on timeout the runner kills that
group by its id, never by name. A case's <id>.toml gives `streams`
("separate" or "merged": stderr into stdout) and optionally
`expect = { nonterminating = true, timeout_s = N }`. Arguments, stdin and
environment come from <id>.args, <id>.stdin and <id>.env. Every run starts in
a fresh temporary working directory, with stdin, stdout and stderr as pipes,
inside a memory cap (LEAN_RUNTIME_CASE_MEM, default 4G, through a systemd user
scope; LEAN_RUNTIME_NO_CAP=1 disables it) and a CPU-time limit.
"""
import argparse, os, pathlib, resource, shlex, shutil, signal, subprocess, sys, tempfile, tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
CASES = ROOT / "tests" / "cases"
TOOLCHAIN = pathlib.Path(os.environ.get(
    "LEAN_TOOLCHAIN", pathlib.Path.home() / ".elan/toolchains/leanprover--lean4---v4.34.0"))
DEFAULT_TIMEOUT = 60
# Every build and run is capped: memory through a systemd user scope
# (MemoryMax, default 4G per run; LEAN_RUNTIME_CASE_MEM), and CPU time through
# RLIMIT_CPU (the run's timeout plus a margin). Address-space limits are not
# used: translated programs reserve large stacks they never touch.
CASE_MEM = os.environ.get("LEAN_RUNTIME_CASE_MEM", "4G")

def capped(cmd):
    if shutil.which("systemd-run") and not os.environ.get("LEAN_RUNTIME_NO_CAP"):
        return ["systemd-run", "--user", "--scope", "--quiet", "--collect",
                "-p", f"MemoryMax={CASE_MEM}", "-p", "MemorySwapMax=0"] + cmd
    return cmd

def cpu_limit(seconds):
    def set_limit():
        resource.setrlimit(resource.RLIMIT_CPU, (seconds, seconds + 5))
    return set_limit

def find_cases(names):
    all_cases = sorted(CASES.glob("**/*.lean"))
    if not names:
        return all_cases
    return [c for c in all_cases if c.stem in names]

def meta(case):
    t = case.with_suffix(".toml")
    return tomllib.loads(t.read_text()) if t.exists() else {}

def build_native(case, outdir):
    c_file = outdir / (case.stem + ".c")
    exe = outdir / case.stem
    subprocess.run(capped([str(TOOLCHAIN / "bin/lean"), "-c", str(c_file), str(case)]),
                   check=True, preexec_fn=cpu_limit(600))
    subprocess.run(capped([str(TOOLCHAIN / "bin/leanc"), str(c_file), "-o", str(exe)]),
                   check=True, preexec_fn=cpu_limit(600))
    return exe

def run(exe, case):
    m = meta(case)
    args = shlex.split(case.with_suffix(".args").read_text()) if case.with_suffix(".args").exists() else []
    stdin = case.with_suffix(".stdin").read_bytes() if case.with_suffix(".stdin").exists() else b""
    env = dict(os.environ, LEAN_BACKTRACE="0")
    if case.with_suffix(".env").exists():
        for line in case.with_suffix(".env").read_text().splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    expect = m.get("expect", {})
    nonterm = expect.get("nonterminating", False)
    timeout = expect.get("timeout_s", 3) if nonterm else DEFAULT_TIMEOUT
    merged = m.get("streams", "separate") == "merged"
    workdir = tempfile.TemporaryDirectory()  # a fresh working directory per run
    def setup():
        cpu_limit(timeout + 30)()
    p = subprocess.Popen(capped([str(exe)] + args), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT if merged else subprocess.PIPE,
                         env=env, cwd=workdir.name, start_new_session=True, preexec_fn=setup)
    try:
        out, err = p.communicate(stdin, timeout=timeout)
        code = str(p.returncode)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)  # the case's own process group only
        out, err = p.communicate()
        code = "timeout"
    workdir.cleanup()
    return out, (err or b""), code

def cmd_expect(ns):
    ok = True
    with tempfile.TemporaryDirectory() as d:
        for case in find_cases(ns.cases):
            exe = build_native(case, pathlib.Path(d))
            results = {run(exe, case) for _ in range(ns.runs)}
            if len(results) != 1:
                print(f"NONDETERMINISTIC {case.stem}: {len(results)} different results in {ns.runs} runs")
                ok = False
                continue
            out, err, code = results.pop()
            case.with_suffix(".out").write_bytes(out)
            case.with_suffix(".err").write_bytes(err)
            case.with_suffix(".code").write_text(code + "\n")
            print(f"recorded {case.stem}: code {code}")
    return 0 if ok else 1

def cmd_check(ns):
    failed = 0
    for case in find_cases(ns.cases):
        exe = pathlib.Path(ns.exe_dir) / case.stem
        if not exe.exists():
            print(f"MISSING {case.stem}")
            failed += 1
            continue
        out, err, code = run(exe, case)
        want = (case.with_suffix(".out").read_bytes(), case.with_suffix(".err").read_bytes(),
                case.with_suffix(".code").read_text().strip())
        if (out, err, code) == want:
            print(f"PASS {case.stem}")
        else:
            diffs = [name for name, got, exp in (("stdout", out, want[0]), ("stderr", err, want[1]),
                                                  ("code", code, want[2])) if got != exp]
            print(f"FAIL {case.stem}: {', '.join(diffs)} differ (code {code}, expected {want[2]})")
            failed += 1
    print(f"{failed} failed")
    return 1 if failed else 0

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("expect"); e.add_argument("--runs", type=int, default=5); e.add_argument("cases", nargs="*")
    c = sub.add_parser("check"); c.add_argument("--exe-dir", required=True); c.add_argument("cases", nargs="*")
    ns = ap.parse_args()
    sys.exit(cmd_expect(ns) if ns.cmd == "expect" else cmd_check(ns))

main()
