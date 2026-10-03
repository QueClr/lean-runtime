# Test cases

One directory per area (`hash/`, `float/`, `string/`, `nat/`, `io/`,
`tasks/`, ...). Every case states what compiled Lean does, and both
translators must reproduce it.

## Two kinds of case

**`program`**: a whole Lean file with a `main (args : List String)`.
- `<id>.lean`, plus optional `<id>.args`, `<id>.stdin` and `<id>.env`;
- `<id>.expected` holds the stdout, stderr and exit code of a native build.

**`row`**: one function on given arguments, in `<area>.rows`, with fields:
- `fn`: a Lean constant;
- `args`: Lean term syntax, one per argument;
- `expected`: Lean's `repr` of the result, or `panic: <message>` together
  with the default value the function returns.

## Fields every case carries

Written in `<id>.toml` for programs and inline for rows:

| Field | Meaning |
|---|---|
| `id`, `area` | Unique name and area |
| `lean_version` | The Lean version of the native build that produced the expected values (4.34.0) |
| `source` | Where the case came from: a finding id, the project that found it, and file:line |
| `normalize` | Optional list of `regex -> replacement` applied to both outputs before comparing (pids, times, addresses) |
| `deviations` | Optional documented translator deviations, e.g. `{ leanrs = "DV15 (d)", lean2rr = "plan §10 ..." }`. The expected value stays compiled Lean's; a listed deviation is the only difference allowed |
| `files` | For IO cases: the expected directory tree after the run, with each file's SHA-256 |

## Rules

- **Inputs come from argv or stdin, never from literals in `main`.** Both
  translators evaluate closed terms at translation time, so a test whose
  values are all literals never reaches the runtime.
- **Output is deterministic.** No pids, times or addresses unless `normalize`
  covers them.
- **Expected values come from compiled Lean 4.34.0, never from `#eval`.**
- **Every bug or disagreement** either translator finds in runtime behaviour
  becomes a case here, even when the fix lands in a translator.
