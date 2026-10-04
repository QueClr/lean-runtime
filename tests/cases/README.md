# Test cases

One directory per area (`hash/`, `float/`, `string/`, `nat/`, `io/`,
`tasks/`, ...). Every case states what compiled Lean does, and both
translators must reproduce it.

## Two kinds of case

**`program`**: a whole Lean file with a `main (args : List String)`.
- `<id>.lean`, plus optional `<id>.args`, `<id>.stdin` and `<id>.env`;
- optional `<id>.pipe`: a bash line run with `pipefail` instead of the
  executable, with `$BIN` (the executable) and `$ARGS` (the arguments), for
  closed descriptors (`$BIN >&-`), `ulimit`, `| head` or `2>&1 | od`;
- optional `<id>.files/`: the starting directory tree, copied into the
  working directory before the run;
- `<id>.out`, `<id>.err` and `<id>.code` hold the stdout, stderr and exit code
  of the native build (for a Lean bug, the correct outcome; see `native`
  below), recorded by `scripts/cases.py expect`, which builds
  natively with Lean 4.34.0 and requires identical results over 5 runs
  (a `schedule_dependent` case records every outcome seen: the most frequent
  as `<id>.out/.err/.code`, the others as `<id>.altK.out/.err/.code`, all of
  them allowed);
- a translator checks its own builds with
  `scripts/cases.py check --exe-dir DIR`, where `DIR/<id>` is its executable
  for each case. With `--translator NAME` (`lean2rr`, `leanrs`), a case whose
  `deviations` give NAME a deviation that names no Lean bug (a `DVnn`)
  reports a missing executable or a different outcome as `DEVIATION`, not
  counted as a failure; a Lean bug (`LB-nn`) excuses nothing.

A program case whose behaviour is a confirmed Lean bug (`docs/lean-bugs.md`)
expects the correct outcome in `<id>.out/.err/.code`, written by hand, and
records native Lean's in its `.toml` as `native = { stdout = "…", stderr =
"…", code = "…" }` (`code` as in `<id>.code`: a status, or `timeout` for a
hang, with `expect = { hang = N }` to bound the run), beside `deviations`
naming the `LB-nn`; `scripts/cases.py expect` then checks native against
`native` (`NATIVE CHANGED` if it differs) and leaves the expected files
alone. Where native's wrong outcome is also nondeterministic, the case has
`hand_written = true` instead of `native`, and `expect` skips it
(`refs/lost_update`). `check` accepts the expected files and the case's
alternatives `<id>.altK.*`, which for such a case are other correct
outcomes, never native's wrong one: both commands fail a case whose
`native` equals its expected files or an alternative (`NATIVE EXPECTED`),
and a case whose `deviations` name an `LB-nn` without `native` or
`hand_written` (`OLD FORM`).

**`row`**: one function call, as a TOML `[[row]]` table in
`<area>/<area>.rows.toml`:
- `fn` (a Lean constant, or a `fun` term over constants, for a result too
  big to print: `fun a e => Nat.log2 (a ^ e)`) with `args` (Lean term
  syntax, one per argument), or `expr` (one closed Lean expression);
- `expected`: Lean's `repr` of the result; for a panic, `panic: <message>`
  (its first line) with `default` (the value returned) and `stderr`; for a
  row that ends the process, `ends`;
- optional `bits = { args = [...], result = "0x…" }` for `Float`/`Float32`
  values, since `repr` loses NaN payloads and `-0.0`;
- optional `ends = { stderr = "…", code = N }` for a row that ends the
  process (`INTERNAL PANIC`, an abort): its whole stderr and its status
  (128 + N for a signal N);
- optional `env = { NAME = "value" }`: variables added to the environment
  of the call's process (`LEAN_ABORT_ON_PANIC`);
- optional `native = { expected = "…", … }` beside a `deviations` that names
  an `LB-nn` of `docs/lean-bugs.md`: native Lean's outcome where the crate
  and both translators deliberately compute the Lean definition's result,
  which is then `expected`;
- optional `sharing = "unique" | "shared" | "both"` for a row with an
  in-place path: Lean's result does not depend on it, but each runtime has
  two code paths.

The oracle that records rows gets its inputs through argv, stdin or an
opaque `IO` identity (`dyn`), so nothing is folded at compile time.

**Recording and running rows.** `scripts/gen_rows.py
tests/cases/<area>/<area>.rows.toml` rewrites every `fn`/`args` row's
`expected`, `default`, `stderr` and `bits.result` from `scripts/oracle`, a
Lean program built natively with Lean 4.34.0 that reads the inputs from
stdin (`--check` only compares; `--toolchain v4.34.0-rc1` compares against
another version). `tests/rows.rs` runs every row against the crate. The
argument terms it reads:
- numerals (`7`, `0xff`), negative ones in parentheses (`(-128)`), string
  literals, character literals (`'é'`, Lean escapes: `'\x00'`), positions
  `⟨5⟩`, `(ByteArray.mk #[1, 2])` and slices
  `(("héllo".toSlice.drop 1).dropEnd 0)`; a fixed-width integer argument is
  the numeral's value in that type, as in Lean;
- a `Float`/`Float32` argument is a Lean term (`0.7`, `(-0.0)`,
  `(1.0 / 0.0)`, `(0.0 / 0.0)`, `(-(0.0 / 0.0))`) whose exact value is the
  next entry of `bits.args` (16 hex digits for `Float`, 8 for `Float32`); the
  only NaNs are the quiet NaN and its negation, the ones Lean can build from
  bits;
- for a panic, `<message>` is the string Lean passes to `lean_panic_fn`, and
  `stderr` is what Lean printed with `LEAN_BACKTRACE=0`.

`tests/rows2.rs` runs the areas nat, int, array, panic and repr, whose
arguments may be numbers of any size, with more terms: a character `'a'`
(Lean escapes: `'\x7f'`, `'\u0080'`), `true`/`false`, an `Array Nat`
`#[1, 2]`, a `FloatArray` `(FloatArray.mk #[1.5, -2.25])` (decimal literals
exact in binary), and `(2 ^ K)` or `(2 ^ K + A)` for a `Nat` too big to
write out. A `ByteArray` or `FloatArray` result is shown as `repr x.toList`,
since neither has a `Repr` instance. `scripts/gen_rows.py` runs a row with
`ends`, `env` or `deviations` alone, in its own oracle process (the author
of an `ends` row writes `ends = {}`); a `deviations` naming an `LB-nn` keeps
the author's `expected` and records native's outcome in `native`, while a
one-translator deviation (leanrs's `DVn`) keeps native's `expected`.

## Fields every case carries

Written in `<id>.toml` for programs and inline for rows:

| Field | Meaning |
|---|---|
| `id`, `area` | Unique name and area |
| `lean_version` | The Lean version of the native build that produced the expected values (4.34.0) |
| `source` | Where the case came from: a finding id and the project that found it, plus file:line where there is one |
| `normalize` | Optional list of `"regex -> replacement"` rules (Python `re`, multi-line mode, on bytes), applied in order to stdout and stderr of every run before recording or comparing (pids, times, addresses, native's backtrace lines) |
| `deviations` | Optional documented deviations, e.g. `{ leanrs = "DV15 (d)", lean2rr = "LB-03" }`. Where one names a Lean bug (`LB-nn`), the expected value is the correct one and native's is in `native` (rows and programs alike). Where none does (a translator's or the shared runtime's own deviation, native being right: leanrs's `DVnn`, `LIO2-05` of the coordination notes, `LQ1-01` of `docs/native-quirks.md`, `LSCHED-01` of `docs/sched.md`), the expected value stays compiled Lean's, a program case keeps the deviating outcome as `<id>.alt1.*`, written by hand, and `check` accepts both |
| `native` | With a `deviations` naming an `LB-nn`: native Lean's outcome, `{ stdout, stderr, code }` for a program, `{ expected, … }` for a row |
| `hand_written` | Optional `true` for a Lean bug whose native outcome is nondeterministic: the expected files are the correct outcome, written by hand, and `expect` skips the case |
| `files` | For IO cases: the expected directory tree after the run, with each file's SHA-256 |
| `streams` | `"separate"` (default) or `"merged"` (stderr into stdout, to observe the order between the two) |
| `schedule_dependent` | Optional `true` when native Lean has more than one outcome depending on thread timing. The case records the dominant native outcome as `<id>.*` and each other native outcome seen as `<id>.altK.*`, and `check` accepts any of them; the `.toml` comment says what they are and how often each was seen. An outcome possible natively but never seen is named in the comment and not accepted (`tasks/dropped_pure_task`): a translator showing it shows another native schedule, not a semantic error |
| `expect` | Optional `{ hang = N }` (N seconds, about 3 to 5) for a program that natively never exits. The output produced before the timeout is compared; the code is `timeout`. In a case with `native`, `hang` only bounds the run (native's, and a translator's): the expected code is the corrected one in `<id>.code`. Each run starts in its own process group, which the runner kills by its id, never by name |

## How a case runs

`scripts/cases.py` runs every case:
- in a fresh temporary working directory (with `<id>.files/` copied in);
- from an empty environment plus `LEAN_BACKTRACE=0` and `<id>.env` (and a
  minimal `PATH` for `.pipe` lines);
- with stdin, stdout and stderr as pipes, so stdout is fully buffered (some
  expectations, such as the order of a merged stream, depend on that);
- inside a memory cap (4G by default) and a CPU-time limit;
- in its own process group, killed by its id on timeout;
- an exit by signal N is recorded as code 128+N.

## Rules

- **Inputs come from argv or stdin, never from literals in `main`.** Both
  translators evaluate closed terms at translation time, so a test whose
  values are all literals never reaches the runtime.
- **Creation of what is tested must be pinned.** A pure value used only on a
  branch the arguments never take is floated into that branch and never
  created; observe it with an effect instead (for a task, `IO.hasFinished`).
- **Output is deterministic.** No pids, times or addresses unless `normalize`
  covers them.
- **Expected values come from compiled Lean 4.34.0, never from `#eval`.**
- **Every bug or disagreement** either translator finds in runtime behaviour
  becomes a case here, even when the fix lands in a translator.
