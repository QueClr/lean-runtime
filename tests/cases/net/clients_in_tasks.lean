import Std.Internal.UV
import Std.Net.Addr
/-! Several tasks use the network at the same time (net-threads): a server
task accepts four connections and echoes each one in a dedicated task of
its own, and four dedicated client tasks each connect, send a name and read
the echo and then the end of the stream. `main` prints the clients' results
in their order. In threads mode each task runs on a thread of its own and
calls the externs while the others do, and the loop thread resolves every
promise. The address comes from argv. -/
open Std.Internal.UV Std.Net

def get {α} [Nonempty α] (p : IO.Promise (Except IO.Error α)) : IO α := do
  match ← IO.wait p.result? with
  | some (.ok v) => pure v
  | some (.error e) => throw e
  | none => throw (IO.userError "dropped")

def text (b : Option ByteArray) : String :=
  match b with
  | none => "the end of the stream"
  | some b => (String.fromUTF8? b).getD "?"

def echo (c : TCP.Socket) : IO Unit := do
  let m ← get (← c.recv? 64)
  get (← c.send #["echo ".toUTF8 ++ m.getD .empty])
  get (← c.shutdown)

def client (ip : IPv4Addr) (port : UInt16) (name : String) : IO String := do
  let c ← TCP.Socket.new
  get (← c.connect (.v4 { addr := ip, port }))
  get (← c.send #[name.toUTF8])
  let r ← get (← c.recv? 64)
  let e ← get (← c.recv? 64)
  return s!"{name}: {text r}, then {text e}"

def main (args : List String) : IO Unit := do
  let ip := (IPv4Addr.ofString args[0]!).get!
  let s ← TCP.Socket.new
  s.bind (.v4 { addr := ip, port := 0 })
  s.listen 16
  let port := (← s.getSockName).port
  let server ← IO.asTask (prio := .dedicated) do
    let mut hs := #[]
    for _ in [0:4] do
      let c ← get (← s.accept)
      hs := hs.push (← IO.asTask (prio := .dedicated) (echo c))
    for h in hs do
      match ← IO.wait h with
      | .ok () => pure ()
      | .error e => throw e
  let clients ← ["ann", "bob", "cyd", "dee"].mapM fun n =>
    IO.asTask (prio := .dedicated) (client ip port n)
  for t in clients do
    match ← IO.wait t with
    | .ok r => IO.println r
    | .error e => IO.println s!"client: {e}"
  match ← IO.wait server with
  | .ok () => IO.println "server done"
  | .error e => IO.println s!"server: {e}"
