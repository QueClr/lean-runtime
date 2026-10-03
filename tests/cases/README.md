# Test cases

One directory per area (`hash/`, `float/`, `string/`, `nat/`, `io/`,
`tasks/`, ...). This format is provisional and will be settled with both
translators.

**Whole programs** (both translators run these end to end):
- `<id>.lean` holds one module with a `main`;
- optional `<id>.args`, `<id>.stdin`, `<id>.env`;
- `<id>.expected` holds `stdout`, `stderr` and the exit code from a native
  Lean 4.34.0 build, with a header naming the Lean version.

**Rows** (unit tests of single functions): `<area>.rows` holds lines of
`function`, `input`, `expected output`. The expected output comes from
compiled Lean 4.34.0, never from `#eval`.

Each case says where it came from: the cross-test, a bug report, or a
translator's own suite.
