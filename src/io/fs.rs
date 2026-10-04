//! The file-system primitives of Lean 4.34.0's `io.cpp` and `process.cpp`:
//! directories, `IO.FS.Metadata`, `realPath`, removal, renaming, hard links,
//! permissions and the working directory.
//!
//! Every primitive refuses a path holding a NUL byte first
//! (`mk_embedded_nul_error`), except `setCurrentDir`, which Lean passes as a C
//! string (the part before the NUL). A failure is decoded as io.cpp decodes
//! it: `decode_io_error(errno, path)` for the C library calls, and
//! `decode_uv_error` for the operations Lean implements with libuv
//! (`metadata`, `symlinkMetadata`, `removeFile`, `hardLink`), which libuv
//! performs with `errno` cleared first (`uv__fs_work`).
//!
//! Results of unbounded size (a path, a directory entry's name) are appended
//! to the caller's [`ByteSink`] or passed to its callback, as bytes; the
//! caller decodes them as Lean's `mk_string` does (lossily).
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/fs.rs` (the mirrors of `FileType`,
//! `SystemTime`, `Metadata`, the rows over std) and lean2rr's
//! `runtime/leanrt/src/fs.rs` (the libuv-based error decoding, the `PATH_MAX`
//! buffers, `setCurrentDir`'s C string, `getCurrentDir` without a file name).

use super::env::PATH_MAX;
use super::error::{set_errno, IoError, ERANGE};
use super::ByteSink;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Lean's `IO.FS.FileType`, its constructors in Lean's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    Dir,
    File,
    Symlink,
    Other,
}

/// Lean's `IO.FS.SystemTime`: whole seconds (an `Int`) and nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemTime {
    pub sec: i64,
    pub nsec: u32,
}

/// Lean's `IO.FS.Metadata`, its fields in Lean's order (`type` as
/// `file_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub accessed: SystemTime,
    pub modified: SystemTime,
    pub byte_size: u64,
    pub file_type: FileType,
    pub num_links: u64,
}

fn os_path(p: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(p))
}

/// The path, or `mk_embedded_nul_error` if it holds a NUL byte.
fn c_path(p: &[u8]) -> Result<&Path, IoError> {
    if p.contains(&0) {
        Err(IoError::embedded_nul(p))
    } else {
        Ok(os_path(p))
    }
}

/// The `errno` of a failed std call, recorded in the model.
fn code(e: &std::io::Error) -> i32 {
    let c = e.raw_os_error().unwrap_or(0);
    set_errno(c);
    c
}

/// A C library call's failure: `decode_io_error(errno, fname)`.
fn c_err(fname: &[u8]) -> impl Fn(std::io::Error) -> IoError + '_ {
    move |e| IoError::decode_io_error(code(&e), Some(fname))
}

/// A libuv call's failure: `decode_uv_error(-errno, fname)`.
fn uv_err(e: &std::io::Error, fname: &[u8]) -> IoError {
    IoError::decode_uv_error(-code(e), Some(fname))
}

/// `IO.FS.createDir` (`lean_io_create_dir`, `mkdir(p, 0777)`).
pub fn create_dir(p: &[u8]) -> Result<(), IoError> {
    std::fs::create_dir(c_path(p)?).map_err(c_err(p))
}

/// `IO.FS.removeDir` (`lean_io_remove_dir`, `rmdir`).
pub fn remove_dir(p: &[u8]) -> Result<(), IoError> {
    std::fs::remove_dir(c_path(p)?).map_err(c_err(p))
}

/// `IO.FS.removeFile` (`lean_io_remove_file`, libuv's `uv_fs_unlink`).
pub fn remove_file(p: &[u8]) -> Result<(), IoError> {
    let path = c_path(p)?;
    set_errno(0);
    std::fs::remove_file(path).map_err(|e| uv_err(&e, p))
}

/// `IO.FS.rename` (`lean_io_rename`, `rename`): the paths are checked in
/// order; an error names both, `<from> and/or <to>`.
pub fn rename(from: &[u8], to: &[u8]) -> Result<(), IoError> {
    let a = c_path(from)?;
    let b = c_path(to)?;
    std::fs::rename(a, b).map_err(|e| {
        let mut both = from.to_vec();
        both.extend_from_slice(b" and/or ");
        both.extend_from_slice(to);
        IoError::decode_io_error(code(&e), Some(&both))
    })
}

/// `IO.FS.hardLink` (`lean_io_hard_link`, libuv's `uv_fs_link`): an error
/// names the original.
pub fn hard_link(orig: &[u8], link: &[u8]) -> Result<(), IoError> {
    let a = c_path(orig)?;
    let b = c_path(link)?;
    set_errno(0);
    std::fs::hard_link(a, b).map_err(|e| uv_err(&e, orig))
}

/// `IO.Prim.setAccessRights` (`lean_chmod`, `chmod(p, mode)`).
pub fn set_access_rights(p: &[u8], mode: u32) -> Result<(), IoError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(c_path(p)?, std::fs::Permissions::from_mode(mode)).map_err(c_err(p))
}

/// `IO.FS.realPath` (`lean_io_realpath`, `realpath` into a `PATH_MAX`
/// buffer), appended to `out`; any failure is `mk_file_not_found_error`
/// (`noFileOrDirectory path 2 ""`).
pub fn real_path<S: ByteSink + ?Sized>(p: &[u8], out: &mut S) -> Result<(), IoError> {
    match std::fs::canonicalize(c_path(p)?) {
        Ok(r) => {
            out.extend_from_slice(r.as_os_str().as_bytes());
            Ok(())
        }
        Err(e) => {
            code(&e);
            Err(IoError::file_not_found(p))
        }
    }
}

/// `System.FilePath.readDir` (`lean_io_read_dir`, `opendir` then `readdir`):
/// `entry` gets each name in `readdir` order, `.` and `..` skipped; an
/// `opendir` failure names the directory; a failing `readdir` ends the listing
/// (Lean does not check it). The caller builds each `DirEntry` from the
/// directory (its argument) and the name.
pub fn read_dir(p: &[u8], mut entry: impl FnMut(&[u8])) -> Result<(), IoError> {
    let dir = std::fs::read_dir(c_path(p)?).map_err(c_err(p))?;
    for e in dir {
        match e {
            Ok(e) => entry(e.file_name().as_bytes()),
            Err(e) => {
                code(&e);
                break;
            }
        }
    }
    Ok(())
}

fn metadata_of(m: &std::fs::Metadata) -> Metadata {
    use std::os::unix::fs::MetadataExt;
    let t = m.file_type();
    Metadata {
        accessed: SystemTime {
            sec: m.atime(),
            nsec: m.atime_nsec() as u32,
        },
        modified: SystemTime {
            sec: m.mtime(),
            nsec: m.mtime_nsec() as u32,
        },
        byte_size: m.size(),
        file_type: if t.is_dir() {
            FileType::Dir
        } else if t.is_file() {
            FileType::File
        } else if t.is_symlink() {
            FileType::Symlink
        } else {
            FileType::Other
        },
        num_links: m.nlink(),
    }
}

/// `System.FilePath.metadata` (`lean_io_metadata`, libuv's `uv_fs_stat`).
pub fn metadata(p: &[u8]) -> Result<Metadata, IoError> {
    let path = c_path(p)?;
    set_errno(0);
    std::fs::metadata(path)
        .map(|m| metadata_of(&m))
        .map_err(|e| uv_err(&e, p))
}

/// `System.FilePath.symlinkMetadata` (`lean_io_symlink_metadata`, libuv's
/// `uv_fs_lstat`).
pub fn symlink_metadata(p: &[u8]) -> Result<Metadata, IoError> {
    let path = c_path(p)?;
    set_errno(0);
    std::fs::symlink_metadata(path)
        .map(|m| metadata_of(&m))
        .map_err(|e| uv_err(&e, p))
}

/// `getcwd` into a `PATH_MAX` buffer, as Lean's C code calls it (a longer
/// path is `ERANGE`); the `errno` on failure.
fn getcwd_max() -> Result<Vec<u8>, i32> {
    match nix::unistd::getcwd() {
        Ok(p) if p.as_os_str().len() < PATH_MAX => Ok(p.into_os_string().into_vec()),
        Ok(_) => {
            set_errno(ERANGE);
            Err(ERANGE)
        }
        Err(e) => {
            set_errno(e as i32);
            Err(e as i32)
        }
    }
}

use std::os::unix::ffi::OsStringExt;

/// `IO.currentDir` (`lean_io_current_dir`, `getcwd` into a `PATH_MAX`
/// buffer), appended to `out`; a failure is Lean's user error `failed to
/// retrieve current working directory`.
pub fn current_dir<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    match getcwd_max() {
        Ok(p) => {
            out.extend_from_slice(&p);
            Ok(())
        }
        Err(_) => Err(IoError::user_error(
            "failed to retrieve current working directory",
        )),
    }
}

/// `IO.Process.getCurrentDir` (`lean_io_process_get_current_dir`, `getcwd`
/// into a `PATH_MAX` buffer), appended to `out`; a failure is decoded without
/// a file name (LB-03: `noFileOrDirectory "" 2 …` in a removed directory,
/// where native crashes).
pub fn process_current_dir<S: ByteSink + ?Sized>(out: &mut S) -> Result<(), IoError> {
    match getcwd_max() {
        Ok(p) => {
            out.extend_from_slice(&p);
            Ok(())
        }
        Err(e) => Err(IoError::decode_io_error(e, None)),
    }
}

/// `IO.Process.setCurrentDir` (`lean_io_process_set_current_dir`, `chdir` of
/// the path as a C string, so up to its first NUL byte); an error names the
/// whole path.
pub fn set_current_dir(p: &[u8]) -> Result<(), IoError> {
    let cut = p.iter().position(|&b| b == 0).unwrap_or(p.len());
    nix::unistd::chdir(os_path(&p[..cut])).map_err(|e| {
        set_errno(e as i32);
        IoError::decode_io_error(e as i32, Some(p))
    })
}
