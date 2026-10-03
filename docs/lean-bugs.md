# Bugs in Lean's own runtime that this crate does not reproduce

lean-runtime mirrors Lean 4.34.0's C runtime, with one exception: where
Lean's runtime is wrong, both translators do the right thing instead. A
behaviour counts as a bug only after a judged verdict:

- the C source lines responsible;
- why it is wrong: the C standard, POSIX, Lean's own documentation or
  evident intent, or lost data or a crash;
- a minimal native repro showing a wrong value, not just an odd symptom.

Anything not confirmed is followed exactly as native does it.

Every confirmed bug has:
- an entry below;
- a case in `tests/cases/` whose `expected` is native's output, with
  `deviations` naming this entry and the correct output as the allowed
  alternative.

Each translator also lists it among its intended differences. Reporting a bug
to Lean upstream is the owner's decision; the entry records the status.

## Entry format

| Field | Content |
|---|---|
| Id | `LB-nn` |
| Summary | One line |
| Where | File and lines in Lean 4.34.0's source (`src/runtime/...`) |
| Why it is a bug | The standard, the documentation or the data loss, quoted where possible |
| Native repro | The case id, and what native prints |
| Our behaviour | What lean-runtime and both translators do instead |
| Translators | lean2rr: plan §10 entry; leanrs: DV id |
| Upstream | Not reported / issue link / known |
| Verdict | Who judged it, and when |

## Confirmed

(none yet)

## Under judgement (2026-10-03)

- **The lost update in `lean_st_ref_get`'s multi-threaded path** (`io.cpp`): a
  concurrent `lean_st_ref_set` may be lost between the read's two exchanges.
  Judge: lean2rr side.
- **Read after write on one FILE drops pending output** (DV20 (a); C11
  7.21.5.3p7). Judge: lean2rr side.
- **DV7:** `LEAN_ABORT_ON_PANIC` aborts and loses stdout's buffer. Judge:
  leanrs side.
- **DV17 (a)–(c):** implementation limits; probably not bugs. Judge: leanrs
  side.
- **DV18 (a):** `getCurrentDir` segfaults after its directory is removed.
  Judge: leanrs side.
- **DV18 (b):** a thunk that forces itself hangs. Judge: leanrs side.
- **DV20 (b):** a second rewind reuses a stale buffer. Judge: leanrs side.

## Not bugs (followed as native)

(none yet)
