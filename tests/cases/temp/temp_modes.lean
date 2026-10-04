/-! The file `createTempFile` makes: created by `mkostemp` with mode 0600 and
`O_CLOEXEC` (libuv's `uv_fs_mkstemp`), opened as `fdopen(fd, "r+")`; the directory
`createTempDir` makes: `mkdtemp`, mode 0700. Both modes are cut by the umask. The new file's
descriptor is not inherited by a child. The random part of each name is never printed. -/

def modeOf (p : System.FilePath) : IO String := do
  let o ← IO.Process.output { cmd := "stat", args := #["-c", "%a", p.toString] }
  return o.stdout.trimAscii.toString

def main : IO Unit := do
  let (h, p) ← IO.FS.createTempFile
  IO.println s!"file mode {← modeOf p}"
  h.putStr "abc"
  h.rewind
  IO.println s!"read back {(← h.read 10).toList}"
  -- the child lists its own descriptors; the temporary file is not among them
  let o ← IO.Process.output { cmd := "sh", args := #["-c", "ls -l /proc/$$/fd | grep -c tmp. || true"] }
  IO.println s!"child sees the file: {o.stdout.trimAscii}"
  IO.FS.removeFile p
  let d ← IO.FS.createTempDir
  IO.println s!"dir mode {← modeOf d}"
  IO.FS.removeDir d
