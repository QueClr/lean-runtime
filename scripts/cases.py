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
a fresh temporary working directory.
"""
import argparse, os, pathlib, shlex, signal, subprocess, sys, tempfile, tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
CASES = ROOT / "tests" / "cases"
TOOLCHAIN = pathlib.Path(os.environ.get(
    "LEAN_TOOLCHAIN", pathlib.Path.home() / ".elan/toolchains/leanprover--lean4---v4.34.0"))
DEFAULT_TIMEOUT = 60

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
    subprocess.run([str(TOOLCHAIN / "bin/lean"), "-c", str(c_file), str(case)], check=True)
    subprocess.run([str(TOOLCHAIN / "bin/leanc"), str(c_file), "-o", str(exe)], check=True)
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
    p = subprocess.Popen([str(exe)] + args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT if merged else subprocess.PIPE,
                         env=env, cwd=workdir.name, start_new_session=True)
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
            print(f"FAIL {case.stem}: code {code} (expected {want[2]})")
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
