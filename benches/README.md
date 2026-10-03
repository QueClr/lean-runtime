# Micro-benchmarks

One benchmark per function of `semantics` (and walks over whole strings for
the position functions), each a pair of programs that print the same
checksum:

- `rust/src/bin/<name>.rs`: the crate's function called as a translator's
  code calls it;
- `native/Bench/<Name>.lean`: the same calls through Lean's own API, built
  natively with Lean 4.34.0.

`scripts/gen_benches.py` generates both sides and `benches.toml` (the list,
with each pair's function and whether it is comparable). The shared parts
are written by hand: `rust/src/lib.rs` and `native/Bench/Harness.lean`.
`scripts/check_benches.sh` builds both sides and checks them; it times
nothing. Timing follows the measurement protocol in `CONTRIBUTING.md`
("Performance") and runs only in an owner-approved session.

## What a binary does

- **Argument.** The one argument is N, the number of iterations. It is the
  only input from argv.
- **Inputs.** Everything else is built before the first clock read and
  passed through an opaque sink (`black_box`, `Bench.pin`), so neither
  compiler can fold it:
  - operand arrays of 4096 floats, drawn with a fixed-seed LCG over each
    function's range, some with special values (NaN, ±inf, ±0, the largest
    finite value, the smallest subnormal);
  - arrays of 4096 string positions;
  - the strings: 64 copies of `"a€😀é0123"` (896 bytes), 896 bytes of ASCII, and 16
    variants of the first.

  The bounds of each range are given by their bits, so Lean's float
  literals never appear in a timed loop. `check_benches.sh` fails if
  `Float.ofScientific` or a big-number literal appears in a generated loop.
- **The loop.** Each iteration steps the LCG and makes one call (a walk over
  a whole string for the `string_walk_*` pairs). Operands are read at run
  time: from the arrays, at an index taken from the LCG, with the bounds
  check the program would make, or from the LCG itself for the integer
  operations. `check_benches.sh` fails if a Rust kernel has no loop left
  after optimization.
- **Output.** Line 1 is the checksum: every result, its bits for a float,
  rotated into a 64-bit accumulator. Line 2 is `kernel_ns <ns>`, the
  monotonic time around the loop.
- **Allocator.** Both sides use mimalloc, as Lean's runtime and both
  translators do.

## The rule for a pair: the same work on both sides

The native twin goes through Lean's normal API path. The Rust twin
therefore does what a translator's generated code does around the crate's
call, the glue included:

- **The program's own code.** A test the Lean program makes to obtain a
  proof (for example `if h : p.atEnd s then … else p.get' s h`) is Lean code,
  which a translator also emits; the Rust twin makes the same test.
- **Boxed arguments and results.** Where Lean's API takes or returns a `Nat`
  or `Int`, the Rust twin passes a `Nat` or `Int` word (`nat_box`,
  `nat_unbox`, `int_box`, `int_unbox`: `2n+1` below 2^63, as Lean's C and
  lean2rr use). Operands stay in the scalar range on both sides: a big
  number would be an allocation in Lean and is the translator's own `Nat`,
  not the crate.
- **New strings.** Where Lean's API returns a new string, the Rust twin makes
  one new string object (`make_string`: a header, the bytes and a NUL in one
  allocation, the characters counted, as `lean_mk_string_from_bytes_unchecked`
  does).
- **Consuming a float.** A float result enters the checksum through
  `Float.toBits`, an out-of-line call in Lean, and through an out-of-line
  `fbits` in Rust.

A pair where this is not possible, because Lean's API allocates where the
crate's result is plain data and the cost depends on each translator's
representation, is marked `comparable = false` with its reason in
`benches.toml` and in both files:
- `float_frexp`, `float32_frexp`: a boxed pair holding a boxed float;
- `string_utf8_get_opt`: the `some`;
- `string_utf8_strlen`: `String.length` reads a cached count.

Smaller asymmetries that remain are noted in each pair's `note`:
- `Array.get!` on an array of strings retains and releases the string in
  Lean, where the Rust twin borrows it;
- a `black_box` on a loop-invariant hash operand in Rust.
