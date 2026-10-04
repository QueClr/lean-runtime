/-
Probe of Lean's IO runtime for lean-runtime's `tests/io_rows.rs`: every expected value there is an
output of this program compiled with Lean 4.34.0. `run.sh` builds it (with `shim.c`, which calls
`lean_decode_io_error` directly) and writes `errno.txt`, `fs.txt`, `buf.txt` and `bufrw.txt`; the
other modes (`env`, `stdin`, `uncaught`, `exit`, `forceexit`, `ret`, `cwd`, `order`) are quoted by
the tests. From leanrs's `rt/leanrs_rt/tests/data/io_probe` (written for v4.34.0-rc1; io.cpp and
process.cpp are unchanged from rc1 to 4.34.0).
-/
instance : Inhabited IO.Error := ⟨.userError ""⟩
@[extern "probe_decode"] opaque probeDecode (e : UInt32) (withPath : Bool) (p : @& String) : IO.Error
@[extern "probe_lossy"] opaque probeLossy (b : @& ByteArray) : String
@[extern "probe_would_overflow"] opaque wouldOverflow (n : USize) : Bool

def optS : Option String → String
  | none => "None" | some s => s!"Some({s.quote})"

def desc : IO.Error → String
  | .alreadyExists f c d => s!"AlreadyExists({optS f}, {c}, {d.quote})"
  | .otherError c d => s!"OtherError({c}, {d.quote})"
  | .resourceBusy c d => s!"ResourceBusy({c}, {d.quote})"
  | .resourceVanished c d => s!"ResourceVanished({c}, {d.quote})"
  | .unsupportedOperation c d => s!"UnsupportedOperation({c}, {d.quote})"
  | .hardwareFault c d => s!"HardwareFault({c}, {d.quote})"
  | .unsatisfiedConstraints c d => s!"UnsatisfiedConstraints({c}, {d.quote})"
  | .illegalOperation c d => s!"IllegalOperation({c}, {d.quote})"
  | .protocolError c d => s!"ProtocolError({c}, {d.quote})"
  | .timeExpired c d => s!"TimeExpired({c}, {d.quote})"
  | .interrupted f c d => s!"Interrupted({f.quote}, {c}, {d.quote})"
  | .noFileOrDirectory f c d => s!"NoFileOrDirectory({f.quote}, {c}, {d.quote})"
  | .invalidArgument f c d => s!"InvalidArgument({optS f}, {c}, {d.quote})"
  | .permissionDenied f c d => s!"PermissionDenied({optS f}, {c}, {d.quote})"
  | .resourceExhausted f c d => s!"ResourceExhausted({optS f}, {c}, {d.quote})"
  | .inappropriateType f c d => s!"InappropriateType({optS f}, {c}, {d.quote})"
  | .noSuchThing f c d => s!"NoSuchThing({optS f}, {c}, {d.quote})"
  | .unexpectedEof => "UnexpectedEof"
  | .userError m => s!"UserError({m.quote})"

def cps (s : String) : String :=
  " ".intercalate (s.toList.map fun c => String.ofList (Nat.toDigits 16 c.toNat)) ++ s!" len={s.length} bytes={s.utf8ByteSize}"

def r {α} (label : String) (sh : α → String) (act : IO α) : IO Unit := do
  try
    let v ← act
    IO.println s!"{label}: ok {sh v}"
  catch e => IO.println s!"{label}: err {desc e}"

def u (_ : Unit) : String := "()"

def main (args : List String) : IO UInt32 := do
  let mode := args.headD ""
  if mode == "errno" then
    for e in List.range 141 ++ [200, 1000, 4095, 4096] do
      let e := e.toUInt32
      let np := if e == 2 || e == 4 then "-" else desc (probeDecode e false "")
      IO.println s!"{e}: {np} | {desc (probeDecode e true "p")}"
    return 0
  if mode == "lossy" then
    let cases : List (List UInt8) := [
      [0x41], [0xC0, 0x80], [0xC0, 0x80, 0x80, 0x41], [0xE2, 0x82], [0xE2, 0x82, 0x41], [0xE2, 0x82, 0xAC],
      [0xED, 0xA0, 0x80], [0xF0, 0x9F, 0x98, 0x80], [0xF0, 0x9F, 0x98], [0xF4, 0x90, 0x80, 0x80],
      [0xF8, 0x80, 0x80, 0x80, 0x80], [0xFF], [0x80], [0x80, 0x80, 0x41], [0x41, 0xFF, 0x42], [0xC2],
      [0xC2, 0x41], [0xE0, 0x80, 0x80], [0xE0, 0xA0, 0x80], [0xF0, 0x80, 0x80, 0x80], [0xC3, 0xA9, 0xFF, 0xC3, 0xA9],
      [0xFE, 0xFF], [0xE2, 0x28, 0xA1], [0xF0, 0x28, 0x8C, 0xBC], [0xF1, 0x80, 0x80], [0], [0x41, 0, 0x42], []]
    for c in cases do
      IO.println s!"{c}: {cps (probeLossy ⟨c.toArray⟩)}"
    return 0
  if mode == "overflow" then
    for n in [0, 1, 2^63 - 1, 2^63, 2^64 - 25, 2^64 - 24, 2^64 - 23, 2^64 - 1] do
      IO.println s!"{n}: {wouldOverflow n.toUSize}"
    return 0
  if mode == "args" then
    for a in args.tail do IO.println (cps a)
    return 0
  if mode == "env" then
    for n in ["LEANRS_PROBE_A", "LEANRS_PROBE_B", "", "LEANRS\x00PROBE_A", "LEANRS_PROBE_A=x", "LEANRS_PROBE_NONE"] do
      let v ← IO.getEnv n
      IO.println s!"{n.quote}: {(v.map cps)}"
    return 0
  if mode == "fs" then
    let d := args.tail.headD "/tmp/leanrs-io-probe"
    r "mk nul" (fun _ => "h") (IO.FS.Handle.mk "a\x00b" .read)
    r "mk missing" (fun _ => "h") (IO.FS.Handle.mk "/nonexistent/x" .read)
    r "mk proc write" (fun _ => "h") (IO.FS.Handle.mk "/proc/version" .write)
    r "createDir tmp" u (IO.FS.createDir "/tmp")
    r "createDir missing parent" u (IO.FS.createDir "/nonexistent/x/y")
    r "createDir nul" u (IO.FS.createDir "a\x00b")
    r "removeFile missing" u (IO.FS.removeFile s!"{d}/missing")
    r "removeDir missing" u (IO.FS.removeDir s!"{d}/missing")
    IO.FS.createDir s!"{d}/full"
    IO.FS.writeFile s!"{d}/full/f" "x"
    r "removeDir full" u (IO.FS.removeDir s!"{d}/full")
    r "removeFile dir" u (IO.FS.removeFile s!"{d}/full")
    r "rename missing" u (IO.FS.rename s!"{d}/missing" s!"{d}/other")
    r "rename nul" u (IO.FS.rename "a\x00b" s!"{d}/other")
    r "rename nul2" u (IO.FS.rename s!"{d}/full/f" "a\x00b")
    r "rename dir onto file" u (IO.FS.rename s!"{d}/full" s!"{d}/full/f")
    r "hardLink missing" u (IO.FS.hardLink s!"{d}/missing" s!"{d}/l")
    r "hardLink exists" u (IO.FS.hardLink s!"{d}/full/f" s!"{d}/full/f")
    r "realPath missing" (fun (p : System.FilePath) => p.toString) (IO.FS.realPath s!"{d}/missing")
    r "realPath ok" (fun (p : System.FilePath) => p.toString) (IO.FS.realPath s!"{d}/full/../full/f")
    r "realPath nul" (fun (p : System.FilePath) => p.toString) (IO.FS.realPath "a\x00b")
    r "metadata missing" (fun _ => "m") (System.FilePath.metadata s!"{d}/missing")
    r "symlinkMetadata missing" (fun _ => "m") (System.FilePath.symlinkMetadata s!"{d}/missing")
    r "metadata notdir" (fun _ => "m") (System.FilePath.metadata s!"{d}/full/f/g")
    r "metadata nul" (fun _ => "m") (System.FilePath.metadata "a\x00b")
    r "metadata file" (fun (m : IO.FS.Metadata) => s!"size={m.byteSize} type={repr m.type} nsec<1e9={decide (m.modified.nsec < 1000000000)}") (System.FilePath.metadata s!"{d}/full/f")
    r "metadata dir" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (System.FilePath.metadata s!"{d}/full")
    r "readDir missing" (fun _ => "a") (System.FilePath.readDir s!"{d}/missing")
    r "readDir file" (fun _ => "a") (System.FilePath.readDir s!"{d}/full/f")
    r "readDir ok" (fun (a : Array IO.FS.DirEntry) => s!"{a.toList.map (fun e => (e.root, e.fileName))}") (System.FilePath.readDir s!"{d}/full")
    r "chmod missing" u (IO.Prim.setAccessRights s!"{d}/missing" 0o644)
    r "setCurrentDir missing" u (IO.Process.setCurrentDir s!"{d}/missing")
    r "setCurrentDir file" u (IO.Process.setCurrentDir s!"{d}/full/f")
    r "dir read" (fun s => s.quote) (do let h ← IO.FS.Handle.mk s!"{d}/full" .read; h.getLine)
    r "dir readbytes" (fun (b : ByteArray) => s!"{b.size}") (do let h ← IO.FS.Handle.mk s!"{d}/full" .read; h.read 10)
    r "dir write" (fun _ => "h") (IO.FS.Handle.mk s!"{d}/full" .write)
    r "writeNew exists" (fun _ => "h") (IO.FS.Handle.mk s!"{d}/full/f" .writeNew)
    r "readWrite missing" (fun _ => "h") (IO.FS.Handle.mk s!"{d}/rwmissing" .readWrite)
    -- handle semantics
    let f := s!"{d}/h.txt"
    IO.FS.writeFile f "line1\nline2\nlast"
    r "getLine seq" (fun (l : List String) => s!"{l.map String.quote}") (do
      let h ← IO.FS.Handle.mk f .read
      let a ← h.getLine; let b ← h.getLine; let c ← h.getLine; let e ← h.getLine; let e2 ← h.getLine
      return [a, b, c, e, e2])
    r "read seq" (fun (l : List Nat) => s!"{l}") (do
      let h ← IO.FS.Handle.mk f .read
      let a ← h.read 0; let b ← h.read 3; let c ← h.read 100; let e ← h.read 5
      return [a.size, b.size, c.size, e.size])
    r "write on read" u (do let h ← IO.FS.Handle.mk f .read; h.putStr "x")
    r "write on read flush" u (do let h ← IO.FS.Handle.mk f .read; h.putStr "x"; h.flush)
    r "read on write" (fun (b : ByteArray) => s!"{b.size}") (do let h ← IO.FS.Handle.mk s!"{d}/w.txt" .write; h.read 4)
    r "getLine on write" (fun s => s.quote) (do let h ← IO.FS.Handle.mk s!"{d}/w.txt" .write; h.getLine)
    r "getLine on append" (fun s => s.quote) (do let h ← IO.FS.Handle.mk s!"{d}/w.txt" .append; h.getLine)
    r "rw interleave" (fun s => s.quote) (do
      IO.FS.writeFile f "abcdef\nghij\n"
      let h ← IO.FS.Handle.mk f .readWrite
      let a ← h.read 2
      h.putStr "XY"
      let b ← h.getLine
      h.putStr "Z"
      h.rewind
      let c ← h.getLine
      let c2 ← h.getLine
      return s!"{a.size}|{b}|{c}|{c2}")
    r "rw file" (fun s => s.quote) (IO.FS.readFile f)
    r "truncate" (fun s => s.quote) (do
      IO.FS.writeFile f "0123456789"
      let h ← IO.FS.Handle.mk f .readWrite
      let _ ← h.read 4
      h.truncate
      let t ← h.getLine
      h.rewind
      let all ← h.getLine
      return s!"{t}|{all}")
    r "truncate write" (fun s => s.quote) (do
      let h ← IO.FS.Handle.mk f .write
      h.putStr "hello"
      h.truncate
      h.putStr "!"
      h.flush
      IO.FS.readFile f)
    r "append" (fun s => s.quote) (do
      IO.FS.writeFile f "ab"
      let h ← IO.FS.Handle.mk f .append
      h.putStr "cd"
      h.rewind
      h.putStr "ef"
      h.flush
      IO.FS.readFile f)
    r "lock" (fun (b : List Bool) => s!"{b}") (do
      let h1 ← IO.FS.Handle.mk f .read
      let h2 ← IO.FS.Handle.mk f .read
      h1.lock
      let a ← h2.tryLock
      let b ← h2.tryLock (exclusive := false)
      h1.unlock
      let c ← h2.tryLock
      h2.unlock
      h1.lock (exclusive := false)
      let e ← h2.tryLock (exclusive := false)
      let g ← h2.tryLock
      return [a, b, c, e, g])
    r "lock released at last use" (fun b => s!"{b}") (do
      let h1 ← IO.FS.Handle.mk f .read
      h1.lock
      let h2 ← IO.FS.Handle.mk f .read
      h2.tryLock)
    r "isTty file" (fun b => s!"{b}") (do let h ← IO.FS.Handle.mk f .read; h.isTty)
    r "random 0" (fun (b : ByteArray) => s!"{b.size}") (IO.getRandomBytes 0)
    r "random 33" (fun (b : ByteArray) => s!"{b.size}") (IO.getRandomBytes 33)
    r "read overflow" (fun (b : ByteArray) => s!"{b.size}") (do let h ← IO.FS.Handle.mk f .read; h.read (USize.ofNat (2^64 - 1)))
    r "stdout read" (fun (b : ByteArray) => s!"{b.size}") (do let s ← IO.getStdout; s.read 4)
    r "stdin write" u (do let s ← IO.getStdin; s.putStr "x")
    r "stdout getLine" (fun s => s.quote) (do let s ← IO.getStdout; s.getLine)
    r "stdout flush" u (do let s ← IO.getStdout; s.flush)
    r "removeDir notdir" u (IO.FS.removeDir s!"{d}/full/f")
    r "createDir file parent" u (IO.FS.createDir s!"{d}/full/f/x")
    r "realPath notdir" (fun (p : System.FilePath) => p.toString) (IO.FS.realPath s!"{d}/full/f/g")
    r "hardLink ok" (fun (m : IO.FS.Metadata) => s!"links={m.numLinks} size={m.byteSize}") (do
      IO.FS.hardLink s!"{d}/full/f" s!"{d}/full/f2"
      System.FilePath.metadata s!"{d}/full/f")
    r "symlinkMetadata link" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (System.FilePath.symlinkMetadata s!"{d}/link")
    r "metadata link" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (System.FilePath.metadata s!"{d}/link")
    r "symlinkMetadata dangling" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (System.FilePath.symlinkMetadata s!"{d}/dangling")
    r "metadata dangling" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (System.FilePath.metadata s!"{d}/dangling")
    r "metadata other" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (System.FilePath.metadata "/dev/null")
    r "readDir lossy" (fun (a : Array IO.FS.DirEntry) => s!"{a.toList.map (fun e => cps e.fileName)}") (System.FilePath.readDir s!"{d}/names")
    r "getLine lossy" (fun (l : List String) => s!"{l.map cps}") (do
      let h ← IO.FS.Handle.mk s!"{d}/bytes.txt" .read
      let a ← h.getLine; let b ← h.getLine; let c ← h.getLine
      return [a, b, c])
    r "removeDir file" u (IO.FS.removeDir s!"{d}/bytes.txt")
    r "rename ok" (fun s => s.quote) (do
      IO.FS.writeFile s!"{d}/r1" "moved"
      IO.FS.rename s!"{d}/r1" s!"{d}/r2"
      IO.FS.readFile s!"{d}/r2")
    r "createDir ok" (fun (m : IO.FS.Metadata) => s!"type={repr m.type}") (do
      IO.FS.createDir s!"{d}/newdir"
      System.FilePath.metadata s!"{d}/newdir")
    r "readBinFile" (fun (b : ByteArray) => s!"{b.toList}") (IO.FS.readBinFile s!"{d}/bytes.txt")
    return 0
  if mode == "uncaught" then
    throw (IO.userError "a\x00b")
  if mode == "uncaught2" then
    IO.print "before"
    let _ ← IO.FS.Handle.mk "/nonexistent/x" .read
    return 0
  if mode == "exit" then
    IO.print "pending"
    let h ← IO.FS.Handle.mk (args.tail.headD "/tmp/iort-probe/exit.txt") .write
    h.putStr "file data"
    IO.Process.exit 7
  if mode == "forceexit" then
    IO.print "pending"
    let h ← IO.FS.Handle.mk (args.tail.headD "/tmp/iort-probe/fexit.txt") .write
    h.putStr "file data"
    IO.Process.forceExit 9
  if mode == "ret" then
    IO.print "x"
    let h ← IO.FS.Handle.mk (args.tail.headD "/tmp/iort-probe/ret.txt") .write
    h.putStr "file data"
    return 300
  if mode == "buf" then
    let target := args.tail.headD "stdout"
    let ops := args.tail.tail
    let h ← if target == "stdout" then pure none else some <$> IO.FS.Handle.mk target .write
    for op in ops do
      if op == "f" then
        match h with
        | none => (← IO.getStdout).flush
        | some h => h.flush
      else
        let n := (op.drop 1).toNat!
        let s := String.ofList (List.replicate n 'x')
        match h with
        | none => IO.print s
        | some h => h.putStr s
    let _ ← IO.Process.forceExit (α := Unit) 0
    match h with
    | none => pure ()
    | some h => h.flush
    return 0
  if mode == "bufrw" then
    let target := args.tail.headD "/tmp/iort-probe/rw.txt"
    let h ← IO.FS.Handle.mk target .readWrite
    for op in args.tail.tail do
      if op == "f" then h.flush
      else if op == "R" then h.rewind
      else if op.startsWith "r" then let _ ← h.read (op.drop 1).toNat!.toUSize
      else
        let n := (op.drop 1).toNat!
        h.putStr (String.ofList (List.replicate n 'x'))
    let _ ← IO.Process.forceExit (α := Unit) 0
    h.flush
    return 0
  if mode == "cwd" then
    let d := args.tail.headD "/tmp/leanrs-io-probe/cwd"
    IO.FS.createDir d
    IO.Process.setCurrentDir d
    r "currentDir ok" (fun (p : System.FilePath) => p.toString) IO.currentDir
    (← IO.getStdout).flush
    r "getCurrentDir ok" (fun (p : System.FilePath) => p.toString) IO.Process.getCurrentDir
    (← IO.getStdout).flush
    IO.FS.removeDir d
    r "currentDir deleted" (fun (p : System.FilePath) => p.toString) IO.currentDir
    (← IO.getStdout).flush
    r "getCurrentDir deleted" (fun (p : System.FilePath) => p.toString) IO.Process.getCurrentDir
    return 0
  if mode == "ref" then
    let r ← IO.mkRef #[1, 2]
    let a ← r.get
    r.modify (·.push 3)
    let b ← r.get
    let old ← r.swap #[9]
    let c ← r.get
    r.set #[7, 7]
    let d ← r.get
    let r2 ← IO.mkRef #[7, 7]
    let r3 := r
    IO.println s!"get={a} modify={b} swap={old} after={c} set={d} ptrEq={← ST.Prim.Ref.ptrEq r r3} ptrEq2={← ST.Prim.Ref.ptrEq r r2}"
    return 0
  if mode == "order" then
    IO.print "a"
    IO.eprint "b"
    IO.print "c"
    IO.eprintln "d"
    IO.print "e"
    throw (IO.userError "boom")
  if mode == "stdin" then
    let s ← IO.getStdin
    let a ← s.getLine; let b ← s.read 3; let c ← s.getLine; let e ← s.getLine; let e2 ← s.getLine
    IO.println s!"{[a, b.size.repr, c, e, e2].map String.quote}"
    return 0
  return 1
