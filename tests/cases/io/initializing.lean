/-! `IO.initializing` (`lean_io_initializing`, io.cpp): Lean's `g_initializing`, true from the
start of the process until the generated `main` calls `lean_io_mark_end_initialization`, right after
the module initializers; false in `main`. An `initialize` block prints it, another stores it. -/

initialize
  IO.println s!"initialize block: {← IO.initializing}"

initialize gDuringInit : Bool ← IO.initializing

def main (args : List String) : IO Unit := do
  IO.println s!"stored during initialization: {gDuringInit}"
  IO.println s!"in main: {← IO.initializing}"
  IO.println s!"args: {args.length}"
