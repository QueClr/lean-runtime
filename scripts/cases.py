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

A requested CASE that no case has is reported as `NO CASE <id>` and fails
the command.

Each run starts in a new process group; on timeout the runner kills that
group by its id, never by name. A case's <id>.toml gives `streams`
("separate" or "merged": stderr into stdout) and optionally
`expect = { nonterminating = true, timeout_s = N }`. Arguments, stdin and
environment come from <id>.args, <id>.stdin and <id>.env; <id>.pipe, if present,
is a bash line run with pipefail instead of the executable ($BIN, $ARGS);
<id>.files/ is copied into the working directory first. Every run starts in
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

def unknown_cases(names):
    """The requested names that no case has (none is skipped silently)."""
    known = {c.stem for c in CASES.glob("**/*.lean")}
    return [n for n in names if n not in known]

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

def expect_of(m):
    e = m.get("expect", {})
    hang = e.get("hang")
    if hang is None and e.get("nonterminating"):
        hang = e.get("timeout_s", 3)
    return hang

def run(exe, case):
    m = meta(case)
    args = shlex.split(case.with_suffix(".args").read_text()) if case.with_suffix(".args").exists() else []
    stdin = case.with_suffix(".stdin").read_bytes() if case.with_suffix(".stdin").exists() else b""
    # An empty environment plus <id>.env (LEAN_BACKTRACE=0 unless <id>.env says
    # otherwise, so panics print no stack trace).
    env = {"LEAN_BACKTRACE": "0"}
    if case.with_suffix(".env").exists():
        for line in case.with_suffix(".env").read_text().splitlines():
            if "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    hang = expect_of(m)
    timeout = hang if hang is not None else DEFAULT_TIMEOUT
    merged = m.get("streams", "separate") == "merged"
    workdir = tempfile.TemporaryDirectory()  # a fresh working directory per run
    files = case.parent / (case.stem + ".files")
    if files.is_dir():
        shutil.copytree(files, workdir.name, dirs_exist_ok=True)
    cmd = [str(exe)] + args
    pipe = case.with_suffix(".pipe")
    if pipe.exists():
        env["BIN"] = str(exe)
        env["ARGS"] = " ".join(shlex.quote(a) for a in args)
        cmd = ["/bin/bash", "-o", "pipefail", "-c", pipe.read_text().strip()]
    def setup():
        cpu_limit(timeout + 30)()
    if pipe.exists():
        env.setdefault("PATH", "/usr/bin:/bin")
    # systemd-run needs the caller's environment; the case itself gets only env.
    cmd = ["env", "-i"] + [f"{k}={v}" for k, v in env.items()] + cmd
    p = subprocess.Popen(capped(cmd), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT if merged else subprocess.PIPE,
                         cwd=workdir.name, start_new_session=True, preexec_fn=setup)
    try:
        out, err = p.communicate(stdin, timeout=timeout)
        # an exit by signal N is recorded as 128+N, as a shell reports it
        code = str(128 - p.returncode if p.returncode < 0 else p.returncode)
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)  # the case's own process group only
        out, err = p.communicate()
        code = "timeout"
    workdir.cleanup()
    return out, (err or b""), code

def cmd_expect(ns):
    ok = True
    for name in unknown_cases(ns.cases):
        print(f"NO CASE {name}")
        ok = False
    with tempfile.TemporaryDirectory() as d:
        for case in find_cases(ns.cases):
            exe = build_native(case, pathlib.Path(d))
            runs = [run(exe, case) for _ in range(ns.runs)]
            distinct = sorted(set(runs), key=runs.index)
            if len(distinct) != 1 and not meta(case).get("schedule_dependent"):
                print(f"NONDETERMINISTIC {case.stem}: {len(distinct)} different results in {ns.runs} runs"
                      " (mark it schedule_dependent to record every outcome)")
                ok = False
                continue
            # The most frequent outcome is the primary one; others are <id>.altK.*
            distinct.sort(key=lambda r: -runs.count(r))
            for old in case.parent.glob(case.stem + ".alt*.*"):
                old.unlink()
            for k, (out, err, code) in enumerate(distinct):
                stem = case.stem if k == 0 else f"{case.stem}.alt{k}"
                (case.parent / (stem + ".out")).write_bytes(out)
                (case.parent / (stem + ".err")).write_bytes(err)
                (case.parent / (stem + ".code")).write_text(code + "\n")
            print(f"recorded {case.stem}: " + ", ".join(
                f"code {c} x{runs.count((o, e, c))}" for o, e, c in distinct))
    return 0 if ok else 1

def cmd_check(ns):
    failed = 0
    for name in unknown_cases(ns.cases):
        print(f"NO CASE {name}")
        failed += 1
    for case in find_cases(ns.cases):
        exe = pathlib.Path(ns.exe_dir) / case.stem
        if not exe.exists():
            print(f"MISSING {case.stem}")
            failed += 1
            continue
        out, err, code = run(exe, case)
        allowed = []
        for stem in [case.stem] + sorted(p.name[:-len(".code")] for p in case.parent.glob(case.stem + ".alt*.code")):
            allowed.append(((case.parent / (stem + ".out")).read_bytes(),
                            (case.parent / (stem + ".err")).read_bytes(),
                            (case.parent / (stem + ".code")).read_text().strip()))
        want = allowed[0]
        if (out, err, code) in allowed:
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
