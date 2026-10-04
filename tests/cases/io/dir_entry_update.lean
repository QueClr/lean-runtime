/-! Chapter 06 A4 fixture A754 (kind `io`): a string literal stored into a runtime mirror's `Str` field
by a structure update, `{ e with fileName := "renamed.lean" }` on an `IO.FS.DirEntry`: the literal was
read at the program's `Rc<Str>` class (`MAIN_CONST_2.get().clone()`) and stored as it was (rustc E0308);
it is now the K4 literal `Str::lit("…")` (chapter 03 T1). Found by the breaker of sysio (eac34312,
U/Dir.lean). -/
def main : IO Unit := do
  IO.FS.createDirAll "d"
  IO.FS.writeFile "d/a.txt" "x"
  let es ← ("d" : System.FilePath).readDir
  match es[0]? with
  | some e =>
    let e2 := { e with fileName := "renamed.lean" }
    let e3 : IO.FS.DirEntry := ⟨"root-lit", "name-lit"⟩
    IO.println s!"{e2.fileName} {decide (e2.root == e.root)} {e2.path.fileName.getD ""} {e3.path}"
  | none => IO.println "none"
  IO.FS.removeFile "d/a.txt"
  IO.FS.removeDir "d"
