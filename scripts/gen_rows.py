#!/usr/bin/env python3
"""Rebuild the expected values of the row cases from native Lean.

    scripts/gen_rows.py [--check] [--toolchain TC] tests/cases/<area>/<area>.rows.toml ...

For every `[[row]]` with `fn` and `args` it sends the call to the row oracle
(`scripts/oracle`, a Lean program built natively with the toolchain TC,
default v4.34.0), which reads the inputs from stdin, and writes back what
Lean printed: `expected` (Lean's `repr`, or `panic: <message>` with `default`
and `stderr`), and `bits.result` for a `Float`/`Float32` result. The inputs
and the other fields are kept; the file is rewritten in a canonical layout.

A `Float`/`Float32` argument is written in `args` as a Lean term (a literal
such as `0.7`, `(-0.0)`, `(1.0 / 0.0)`, `(0.0 / 0.0)`), and its exact bits are
the next entry of `bits.args` (16 hex digits for `Float`, 8 for `Float32`);
the oracle receives the bits (a negative NaN as the negation of the quiet NaN,
the only NaNs Lean can build from bits).

Batch 2's argument terms (wire kinds in scripts/oracle/Oracle.lean): a
character literal `'a'`, `true`/`false`, an `Array Nat` `#[1, 2]`, a
`FloatArray` `(FloatArray.mk #[1.5, -2.0])` (decimal literals, exact in
binary), and `(2 ^ K)`, `(2 ^ K + A)`, `(2 ^ K - A)` for numbers too big to
write out, which the oracle builds with shifts.

A row with `ends`, `env` or `deviations` runs alone, in its own oracle
process with `env` added to the environment:
- `ends` (written as `ends = {}` by the row's author): the call ends the
  process; `expected` becomes `ends` and `ends` the process's stderr and
  exit code (128 + N for a signal N);
- `deviations` naming an `LB-nn` of docs/lean-bugs.md: both translators
  and the crate deliberately differ from native here; `expected` is the
  author's (the definition's result) and native's outcome goes to `native`.
  A deviation of one translator only (leanrs's `DVn`) leaves `expected`
  native's: the crate follows native, and the row only records that this
  translator differs.

--check   rewrite nothing; report the rows whose expected values differ and
          exit 1 if any does (to compare toolchains, e.g. `--toolchain
          v4.34.0-rc1`).

The oracle is copied to and built in $LEAN_RUNTIME_ORACLE_DIR/<toolchain>
(default target/oracle/<toolchain>) with elan's `lake`. The build and the run
are capped as in scripts/check.sh: a systemd user scope with
MemoryMax=$LEAN_RUNTIME_MEM (default 16G), unless LEAN_RUNTIME_NO_CAP=1;
LEAN_RUNTIME_WRAP, if set, is a command prefix used instead (another cap).
"""

import argparse
import os
import pathlib
import re
import shlex
import shutil
import struct
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
ORACLE_SRC = ROOT / "scripts" / "oracle"
FIELD_ORDER = ["id", "area", "fn", "args", "expr", "env", "bits", "expected", "default",
               "stderr", "ends", "sharing", "lean_version", "source", "deviations", "native"]


class RowError(Exception):
    pass


# ---------------------------------------------------------------- Lean terms


def parse_string_literal(text, i, quote='"'):
    """A Lean string (or, with quote="'", character) literal at text[i] == quote: the
    string and the index after it."""
    i += 1
    out = []
    simple = {"\\": "\\", '"': '"', "'": "'", "n": "\n", "t": "\t", "r": "\r"}
    while True:
        if i >= len(text):
            raise RowError("unterminated string literal")
        c = text[i]
        if c == quote:
            return "".join(out), i + 1
        if c == "\\":
            e = text[i + 1]
            if e in simple:
                out.append(simple[e])
                i += 2
            elif e == "x":
                out.append(chr(int(text[i + 2 : i + 4], 16)))
                i += 4
            elif e == "u":
                out.append(chr(int(text[i + 2 : i + 6], 16)))
                i += 6
            else:
                raise RowError(f"unknown escape \\{e}")
        else:
            out.append(c)
            i += 1


NAT = r"(0x[0-9a-fA-F]+|[0-9]+)"
FLOAT_CORE = re.compile(r"^([0-9]+\.[0-9]+(e-?[0-9]+)?|[0-9]+e-?[0-9]+|[01]\.0 / 0\.0)$")


def is_float_term(term):
    """Whether an argument term denotes a float: a decimal literal, maybe
    negated and parenthesized, or `0.0 / 0.0`, `1.0 / 0.0` and their negations."""
    t = term.strip()
    while True:
        if t.startswith("(") and t.endswith(")"):
            t = t[1:-1].strip()
        elif t.startswith("-"):
            t = t[1:].strip()
        else:
            return bool(FLOAT_CORE.match(t))


def wire_of(term, float_bits):
    """The oracle's encoding of one argument term (see scripts/oracle/Oracle.lean)."""
    t = term.strip()
    if is_float_term(t):
        if not float_bits:
            raise RowError(f"no bits.args entry for the float argument {t}")
        b = float_bits.pop(0)
        hexd = b[2:] if b.startswith("0x") else b
        if len(hexd) not in (16, 8):
            raise RowError(f"bits must have 8 or 16 hex digits: {b}")
        v, wide = int(hexd, 16), len(hexd) == 16
        sign = 1 << (63 if wide else 31)
        exp = 0x7FF0000000000000 if wide else 0x7F800000
        quiet = 0x7FF8000000000000 if wide else 0x7FC00000
        mag = v & ~sign
        if mag > exp and mag != quiet:
            # Float.ofBits makes every NaN the quiet one; negation only flips the sign
            raise RowError(f"the oracle can only build the quiet NaN and its negation: {b}")
        if v & sign and mag > exp:
            # a negative NaN: the negation of Float.ofBits, which keeps the sign bit
            return ("F:" if wide else "G:") + "%0*x" % (len(hexd), mag)
        return ("f:" if wide else "g:") + hexd
    if t.startswith('"'):
        s, j = parse_string_literal(t, 0)
        if j != len(t):
            raise RowError(f"trailing text after a string literal: {t}")
        return "s:" + s.encode("utf-8").hex()
    if t.startswith('(("'):
        s, j = parse_string_literal(t, 2)
        m = re.compile(r"\.toSlice\.drop " + NAT + r"\)\.dropEnd " + NAT + r"\)$").match(t, j)
        if not m:
            raise RowError(f"bad slice term: {t}")
        return "l:%s:%d:%d" % (s.encode("utf-8").hex(), int(m.group(1), 0), int(m.group(2), 0))
    m = re.fullmatch(r"⟨" + NAT + r"⟩", t)
    if m:
        return "n:%d" % int(m.group(1), 0)
    m = re.fullmatch(r"\(-" + NAT + r"\)", t)
    if m:
        return "i:-%d" % int(m.group(1), 0)
    m = re.fullmatch(r"\(ByteArray\.mk #\[([0-9xa-fA-F, ]*)\]\)", t)
    if m:
        bs = [int(b.strip(), 0) for b in m.group(1).split(",") if b.strip()]
        return "y:" + bytes(bs).hex()
    if t.startswith("'"):
        s, j = parse_string_literal(t, 0, quote="'")
        if j != len(t) or len(s) != 1:
            raise RowError(f"bad character literal: {t}")
        return "c:%x" % ord(s)
    if t in ("true", "false"):
        return "b:%d" % (t == "true")
    m = re.fullmatch(r"#\[([0-9, ]*)\]", t)
    if m:
        return "a:" + ",".join(str(int(x)) for x in m.group(1).split(",") if x.strip())
    m = re.fullmatch(r"\(FloatArray\.mk #\[([-0-9., e]*)\]\)", t)
    if m:
        xs = [x.strip() for x in m.group(1).split(",") if x.strip()]
        return "fa:" + ",".join("%016x" % struct.unpack("<Q", struct.pack("<d", float(x)))[0]
                                for x in xs)
    m = re.fullmatch(r"\(2 \^ ([0-9]+)(?: ([+-]) ([0-9]+))?\)", t)
    if m:
        k, sign, a = int(m.group(1)), m.group(2), int(m.group(3) or 0)
        if k == 0:
            raise RowError(f"use a numeral for {t}")
        return "P:%d:%d" % (k, -a if sign == "-" else a)
    m = re.fullmatch(NAT, t)
    if m:
        return "n:%d" % int(m.group(1), 0)
    raise RowError(f"cannot parse argument: {t}")


def request(row):
    if "fn" not in row:
        raise RowError(f"{row.get('id')}: only fn + args rows are generated")
    float_bits = list(row.get("bits", {}).get("args", []))
    wire = [wire_of(a, float_bits) for a in row.get("args", [])]
    if float_bits:
        raise RowError(f"{row['id']}: more bits.args than float arguments")
    return [row["fn"]] + wire


# ---------------------------------------------------------------- TOML


def toml_str(s):
    """A basic string, or a literal string when that avoids escapes."""
    plain = all(c >= " " and c != "\x7f" for c in s)
    if plain and ('"' in s or "\\" in s) and "'" not in s:
        return "'" + s + "'"
    out = ['"']
    for c in s:
        if c == '"':
            out.append('\\"')
        elif c == "\\":
            out.append("\\\\")
        elif c == "\n":
            out.append("\\n")
        elif c == "\t":
            out.append("\\t")
        elif c < " " or c == "\x7f":
            out.append("\\u%04x" % ord(c))
        else:
            out.append(c)
    out.append('"')
    return "".join(out)


def toml_value(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, str):
        return toml_str(v)
    if isinstance(v, list):
        return "[" + ", ".join(toml_value(x) for x in v) + "]"
    if isinstance(v, dict):
        if not v:
            return "{}"
        return "{ " + ", ".join(f"{k} = {toml_value(x)}" for k, x in v.items()) + " }"
    raise RowError(f"cannot write {v!r}")


def ordered(row):
    keys = [k for k in FIELD_ORDER if k in row] + [k for k in row if k not in FIELD_ORDER]
    return {k: row[k] for k in keys}


def write_rows(path, header, rows):
    out = [header.rstrip("\n"), ""] if header.strip() else []
    for row in rows:
        out.append("[[row]]")
        out.extend(f"{k} = {toml_value(v)}" for k, v in ordered(row).items())
        out.append("")
    path.write_text("\n".join(out), encoding="utf-8")


def read_rows(path):
    text = path.read_text(encoding="utf-8")
    header = text.split("[[row]]", 1)[0]
    rows = tomllib.loads(text).get("row", [])
    seen = set()
    for row in rows:
        if row["id"] in seen:
            raise RowError(f"{path}: duplicate id {row['id']}")
        seen.add(row["id"])
    return header, rows


# ---------------------------------------------------------------- the oracle


def capped_prefix():
    """The command prefix that caps a build or run's memory (scripts/check.sh's `capped`)."""
    if os.environ.get("LEAN_RUNTIME_WRAP"):
        return shlex.split(os.environ["LEAN_RUNTIME_WRAP"])
    if os.environ.get("LEAN_RUNTIME_NO_CAP") == "1" or not shutil.which("systemd-run"):
        return []
    mem = os.environ.get("LEAN_RUNTIME_MEM", "16G")
    return ["systemd-run", "--user", "--scope", "--quiet", "--collect",
            "-p", f"MemoryMax={mem}", "-p", "MemorySwapMax=0"]


def oracle_binary(toolchain):
    base = pathlib.Path(os.environ.get("LEAN_RUNTIME_ORACLE_DIR", ROOT / "target" / "oracle"))
    build = base / toolchain
    build.mkdir(parents=True, exist_ok=True)
    for name in ("Oracle.lean", "lakefile.toml"):
        src, dst = ORACLE_SRC / name, build / name
        if not dst.exists() or dst.read_bytes() != src.read_bytes():
            shutil.copyfile(src, dst)
    (build / "lean-toolchain").write_text(f"leanprover/lean4:{toolchain}\n")
    wrap = capped_prefix()
    subprocess.run(wrap + ["lake", "build"], cwd=build, check=True, stdout=sys.stderr)
    return wrap, build / ".lake" / "build" / "bin" / "oracle"


def parse_answers(out):
    """The answers in the oracle's stdout: (stderr lines, float result bits, repr) each."""
    results, panics, bits = [], [], []
    for line in out.split("\n"):
        if line.startswith("@panic "):
            panics.append(line[len("@panic ") :])
        elif line.startswith("@bits "):
            bits.append(line[len("@bits ") :])
        elif line.startswith("=> "):
            results.append({"panics": panics, "bits": bits, "value": line[3:]})
            panics, bits = [], []
    return results


def runs_alone(row):
    return any(k in row for k in ("ends", "env", "deviations"))


def run_oracle(toolchain, rows):
    """One answer per row: {"panics", "bits", "value"}, or {"ends": {"stderr", "code"}}
    for a row whose call ended the process. The rows that may end it (`runs_alone`) each
    get their own process."""
    wrap, exe = oracle_binary(toolchain)
    base_env = dict(os.environ, LEAN_BACKTRACE="0")
    batch = [r for r in rows if not runs_alone(r)]
    stdin = "".join("\t".join(request(r)) + "\n" for r in batch)
    out = subprocess.run(
        wrap + [str(exe)], input=stdin, capture_output=True, text=True, env=base_env, check=True
    ).stdout
    answers = parse_answers(out)
    if len(answers) != len(batch):
        raise RowError(f"oracle answered {len(answers)} of {len(batch)} requests")
    by_id = {r["id"]: a for r, a in zip(batch, answers)}
    for r in rows:
        if not runs_alone(r):
            continue
        env = dict(base_env, **r.get("env", {}))
        p = subprocess.run(wrap + [str(exe)], input="\t".join(request(r)) + "\n",
                           capture_output=True, text=True, env=env)
        got = parse_answers(p.stdout)
        if p.returncode == 0 and len(got) == 1:
            by_id[r["id"]] = got[0]
        elif p.returncode != 0 and not got:
            code = p.returncode if p.returncode >= 0 else 128 - p.returncode
            by_id[r["id"]] = {"ends": {"stderr": p.stderr, "code": code}}
        else:
            raise RowError(f"{r['id']}: oracle exit {p.returncode}, answers {got}")
    return [by_id[r["id"]] for r in rows]


def outcome(row, result):
    """The expected fields for one answer."""
    new = {}
    if "ends" in result:
        new["expected"] = "ends"
        new["ends"] = result["ends"]
        return new
    panics, bits, value = result["panics"], result["bits"], result["value"]
    if panics:
        new["expected"] = "panic: " + panics[0]
        new["default"] = value
        new["stderr"] = "".join(p + "\n" for p in panics)
    else:
        new["expected"] = value
    if bits:
        if len(bits) != 1:
            raise RowError(f"{row['id']}: more than one float in the result")
        new["bits_result"] = bits[0]
    return new


def shared_deviation(row):
    """Whether the row's deviation is the crate's own (an `LB-nn`), so `expected` is the
    definition's result rather than native's."""
    return any(str(v).startswith("LB-") for v in row.get("deviations", {}).values())


def apply(row, result):
    """The row with the oracle's answer in its expected fields, or, for a row with an
    `LB-nn` deviation, in `native`."""
    got = outcome(row, result)
    if shared_deviation(row):
        new = dict(row)
        native = {k: v for k, v in got.items() if k not in ("ends", "bits_result")}
        if "ends" in got:
            native.update(got["ends"])
        if "bits_result" in got:
            native["bits"] = got["bits_result"]
        new["native"] = native
        return ordered(new)
    new = {k: v for k, v in row.items() if k not in ("expected", "default", "stderr", "ends")}
    new.update({k: v for k, v in got.items() if k != "bits_result"})
    b = dict(row.get("bits", {}))
    b.pop("result", None)
    if "bits_result" in got:
        b["result"] = got["bits_result"]
    if b:
        new["bits"] = b
    else:
        new.pop("bits", None)
    return ordered(new)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--toolchain", default="v4.34.0")
    ap.add_argument("files", nargs="+", type=pathlib.Path)
    opts = ap.parse_args()
    any_differ = False
    fields = ("expected", "default", "stderr", "bits", "ends", "native")
    for path in opts.files:
        header, rows = read_rows(path)
        results = run_oracle(opts.toolchain, rows)
        bad = [(r["id"], res["value"]) for r, res in zip(rows, results)
               if res.get("value", "").startswith("!")]
        if bad:
            for rid, res in bad:
                print(f"{path}: {rid}: {res}", file=sys.stderr)
            sys.exit(2)
        new_rows = [apply(r, res) for r, res in zip(rows, results)]
        differ = 0
        for old, new in zip(rows, new_rows):
            if any(old.get(f) != new.get(f) for f in fields):
                differ += 1
                if opts.check:
                    print(f"{path}: {old['id']}: file {[old.get(f) for f in fields]}, "
                          f"{opts.toolchain} {[new.get(f) for f in fields]}")
        if not opts.check:
            write_rows(path, header, new_rows)
        print(f"{path}: {len(rows)} rows, {differ} changed", file=sys.stderr)
        any_differ = any_differ or differ > 0
    if opts.check and any_differ:
        sys.exit(1)


if __name__ == "__main__":
    try:
        main()
    except RowError as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(2)
