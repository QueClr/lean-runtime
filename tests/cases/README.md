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
  of the native build, recorded by `scripts/cases.py expect`, which builds
  natively with Lean 4.34.0 and requires identical results over 5 runs
  (a `schedule_dependent` case records every outcome seen: the most frequent
  as `<id>.out/.err/.code`, the others as `<id>.altK.out/.err/.code`, all of
  them allowed);
- a translator checks its own builds with
  `scripts/cases.py check --exe-dir DIR`, where `DIR/<id>` is its executable
  for each case.

**`row`**: one function call, as a TOML `[[row]]` table in
`<area>/<area>.rows.toml`:
- `fn` (a Lean constant) with `args` (Lean term syntax, one per argument),
  or `expr` (one closed Lean expression);
- `expected`: Lean's `repr` of the result; for a panic, `panic: <message>`
  with `default` (the value returned) and `stderr`;
- optional `bits = { args = [...], result = "0x…" }` for `Float`/`Float32`
  values, since `repr` loses NaN payloads and `-0.0`;
- optional `ends = { stderr = "…", code = N }` for a row that ends the
  process (`INTERNAL PANIC`);
- optional `sharing = "unique" | "shared" | "both"` for a row with an
  in-place path: Lean's result does not depend on it, but each runtime has
  two code paths.

The oracle that records rows gets its inputs through argv, stdin or an
opaque `IO` identity (`dyn`), so nothing is folded at compile time.

## Fields every case carries

Written in `<id>.toml` for programs and inline for rows:

| Field | Meaning |
|---|---|
| `id`, `area` | Unique name and area |
| `lean_version` | The Lean version of the native build that produced the expected values (4.34.0) |
| `source` | Where the case came from: a finding id and the project that found it, plus file:line where there is one |
| `normalize` | Optional list of `regex -> replacement` applied to both outputs before comparing (pids, times, addresses) |
| `deviations` | Optional documented translator deviations, e.g. `{ leanrs = "DV15 (d)", lean2rr = "plan §10 ..." }`. The expected value stays compiled Lean's; a listed deviation is the only difference allowed |
| `files` | For IO cases: the expected directory tree after the run, with each file's SHA-256 |
| `streams` | `"separate"` (default) or `"merged"` (stderr into stdout, to observe the order between the two) |
| `schedule_dependent` | Optional `true` when native Lean has more than one outcome depending on thread timing. The case records the dominant native outcome (the one every measured native run took); the `.toml` comment says what the other outcome is. A translator showing the other outcome shows another native schedule, not a semantic error |
| `expect` | Optional `{ hang = N }` (N seconds, about 3 to 5) for a program that natively never exits. The output produced before the timeout is compared; the code is `timeout`. Each run starts in its own process group, which the runner kills by its id, never by name |

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
