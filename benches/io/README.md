# io micro-benchmarks

One pair of programs per hot path of `lean_runtime::io`, each pair printing
the same checksum:

| Pair | What one iteration does |
|---|---|
| `io_put_str` | `Handle.putStr` of a string of 1 to 64 bytes (chosen by the LCG) on a handle over `/dev/null`: the copy into the `FILE` buffer, one `write` per full buffer |
| `io_read` | `Handle.read k`, `k` from 1 to 64, from a 1 MiB file, rewinding at its end: a new byte array of `k` bytes, the copy out of the buffer, one `read` per buffer |
| `io_get_line` | `Handle.getLine` over a 64 KiB file of lines of 1 to 79 bytes, rewinding at its end: the line into a new string (checked as UTF-8, its characters counted), one `read` per buffer |

- `rust/src/bin/<name>.rs`: the crate's calls as a translator's code makes
  them, with its glue: a new `Vec` of `k` bytes for each `read`, a new `Vec`
  for each line, validated and counted as Lean's `mk_string` does;
- `native/Bench/<Name>.lean`: the same calls through Lean's API, built
  natively with Lean 4.34.0 (`lake build`, release).

`scripts/check_io_benches.sh [N]` builds both sides and checks that each pair
prints the same checksum; it times nothing. Timing follows the measurement
protocol in `CONTRIBUTING.md` ("Performance") and runs only in an
owner-approved session.

## What a binary does

The conventions of the semantics benchmarks: the one argument is N; the
inputs (the strings, the files in the system's temporary directory, named
after the process) are built before the first clock read and pinned
(`black_box`, `Bench.pin`); line 1 is the checksum (every result rotated into
a 64-bit accumulator), line 2 `kernel_ns <ns>`; the input files are removed
after the second clock read. Both sides allocate with mimalloc.

## Asymmetries left (the same work otherwise)

- **Lean's IO calling convention.** Each native IO primitive returns an
  allocated IO result object; a translator's code gets a plain `Result`. This
  is Lean's API path, as the boxing of a `Nat` is, and counts on the native
  side.
- **`getLine`'s copy.** Native Lean builds the line in a `std::string` with
  one `getc` per byte and then copies it into a new string; the crate appends
  the bytes up to the newline with one copy into the caller's object.
