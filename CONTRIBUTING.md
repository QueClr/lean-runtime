# Contributing

## Sequence

1. Cross-test the two runtimes, and fix each disagreement in whichever
   runtime is wrong.
2. Move both translators to Lean 4.34.0.
3. Extract `semantics`, then `io`, then `sched`. For each module, keep the
   implementation the cross-tests show correct, and the safe one where both
   are. Both translators' test suites gate every step.

Until a module is extracted, new ports of Lean runtime functions are written
here, not in either translator, unless one of them needs the function at
once.

## Code

- **No `unsafe` in the default build.** Anything that needs `unsafe` comes
  from a vetted external crate: `nix` or `rustix` for system calls, a
  coroutine crate for task switching.
- **`unsafe-fast`.** Every `unsafe` item has its own `#[allow(unsafe_code)]` (the crate root denies it under this feature). An implementation behind this feature must:
  - have the same observable behaviour as the safe one, which stays;
  - have an entry in `UNSAFE.md` with a written proof;
  - run under Miri (`scripts/check.sh` runs Miri on the `unsafe-fast` configurations), and under Kani where that applies (Kani joins the checks with the first entry).

  The tests run both configurations.
- **Signatures on views and plain data.** No function here owns, allocates
  or frees a Lean value of either translator.
- **Builds.** Offline, no nightly features, and a plain `rustc --crate-type
  rlib` build must work (one translator does not use cargo). Dependencies
  are vendored under `vendor/`.
- **Comments.** Each function names the Lean C function or Lean definition
  it mirrors.

## Tests

- `tests/cases/` holds one case per behaviour, with its expected output
  from a native Lean 4.34.0 build (format in `tests/cases/README.md`).
- **Every bug or disagreement** either translator finds in its runtime
  becomes a case here, if it concerns runtime behaviour, even when the fix
  lands in a translator.
- Unit tests next to the code use values from the same cases.

## Commits

Small commits with a plain description of the change and its reason.
Each translator moves its pinned commit after its own suite passes.
