/-! `handoff_then_sync_map` with the pipe handle inside a tree: the handle is
dropped when the tree is, after `Tree.count`'s last use of it (in a
translator, inside the drain of the tree's free). Natively the same:
"dep 5 7", then "main after mapTask". -/
inductive Tree where
  | node (h : Option IO.FS.Handle) (kids : Array Tree)

@[noinline] def Tree.count : Tree → Nat
  | .node _ ks => ks.size

def main : IO Unit := do
  let t ← BaseIO.asTask (do IO.sleep 300; pure (5 : Nat))
  let slow ← BaseIO.asTask (do IO.sleep 1500; pure (7 : Nat))
  let child ← IO.Process.spawn
    { cmd := "sh", args := #["-c", "sleep 1; cat > /dev/null"], stdin := .piped }
  let (stdin, child) ← child.takeStdin
  stdin.write (ByteArray.mk (Array.replicate 65536 120))
  stdin.flush
  stdin.putStr "x"
  let tr := Tree.node none #[Tree.node (some stdin) #[]]
  if tr.count == 7 then IO.println "never"
  let d ← IO.mapTask (sync := true) (fun v => do
      let w ← IO.wait slow
      IO.println s!"dep {v} {w}") t
  IO.println "main after mapTask"
  let _ ← IO.wait d
  let _ ← child.wait
  pure ()
