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
use super::error::{set_errno, IoError, EINVAL, ENAMETOOLONG, ERANGE};
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
    super::effect_point();
    std::fs::create_dir(c_path(p)?).map_err(c_err(p))
}

/// `IO.FS.removeDir` (`lean_io_remove_dir`, `rmdir`).
pub fn remove_dir(p: &[u8]) -> Result<(), IoError> {
    super::effect_point();
    std::fs::remove_dir(c_path(p)?).map_err(c_err(p))
}

/// `IO.FS.removeFile` (`lean_io_remove_file`, libuv's `uv_fs_unlink`).
pub fn remove_file(p: &[u8]) -> Result<(), IoError> {
    super::effect_point();
    let path = c_path(p)?;
    set_errno(0);
    std::fs::remove_file(path).map_err(|e| uv_err(&e, p))
}

/// `IO.FS.rename` (`lean_io_rename`, `rename`): the paths are checked in
/// order; an error names both, `<from> and/or <to>`.
pub fn rename(from: &[u8], to: &[u8]) -> Result<(), IoError> {
    super::effect_point();
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
    super::effect_point();
    let a = c_path(orig)?;
    let b = c_path(link)?;
    set_errno(0);
    std::fs::hard_link(a, b).map_err(|e| uv_err(&e, orig))
}

/// `IO.Prim.setAccessRights` (`lean_chmod`, `chmod(p, mode)`).
pub fn set_access_rights(p: &[u8], mode: u32) -> Result<(), IoError> {
    super::effect_point();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(c_path(p)?, std::fs::Permissions::from_mode(mode)).map_err(c_err(p))
}

/// `IO.FS.realPath` (`lean_io_realpath`), appended to `out`. A path holding a
/// NUL byte is `mk_embedded_nul_error` first. Then Lean calls `realpath(path,
/// buffer)` with a `PATH_MAX` buffer: glibc (`stdlib/canonicalize.c`) fails
/// with `ENAMETOOLONG` when the result and its NUL exceed `PATH_MAX`, so a
/// resolved path of 4096 bytes or more fails (POSIX allows it; Lean's docs
/// tie `realPath` to POSIX `realpath`). `std::fs::canonicalize` runs the same
/// resolution without a buffer, so the length is checked here. Every failure,
/// whatever the `errno` (`ENOENT`, `EACCES`, `ELOOP`, `ENOTDIR`,
/// `ENAMETOOLONG`, ...), is `mk_file_not_found_error`: `noFileOrDirectory path
/// 2 ""`, with `realpath`'s own code as the modelled `errno`. A success leaves
/// it at `EINVAL` when glibc's walk called `readlink` on a component that is
/// not a symbolic link (`walk_reads_non_link`; leanrs review F2), else at
/// `ERANGE` for a relative path in a working directory of 1024 bytes or more
/// (glibc's first `getcwd` into a 1024-byte buffer; review RIO1-16).
pub fn real_path<S: ByteSink + ?Sized>(p: &[u8], out: &mut S) -> Result<(), IoError> {
    let path = c_path(p)?;
    match std::fs::canonicalize(path) {
        Ok(r) if r.as_os_str().len() < PATH_MAX => {
            // a relative path starts from `getcwd` into glibc's 1024-byte
            // scratch buffer, which fails with ERANGE first for a longer
            // working directory (then grows; review RIO1-16)
            if path.is_relative()
                && std::env::current_dir().is_ok_and(|d| d.as_os_str().len() + 1 > 1024)
            {
                set_errno(ERANGE);
            }
            if walk_reads_non_link(path) {
                set_errno(EINVAL);
            }
            out.extend_from_slice(r.as_os_str().as_bytes());
            Ok(())
        }
        Ok(_) => {
            set_errno(ENAMETOOLONG);
            Err(IoError::file_not_found(p))
        }
        Err(e) => {
            code(&e);
            Err(IoError::file_not_found(p))
        }
    }
}

/// Whether glibc's `realpath` (`stdlib/canonicalize.c`) calls `readlink` on a
/// component that is not a symbolic link while it resolves `path`, which
/// leaves `errno` at `EINVAL` when it succeeds. It walks from `/`, or from the
/// working directory for a relative path (whose own components it does not
/// read): `.` is dropped, `..` goes back one component without a `readlink`,
/// every other component is read, a symbolic link's target replacing it (from
/// `/` when the target is absolute). Called after `realpath` succeeded; a
/// failing `readlink` other than `EINVAL` (a race) stops the walk. From
/// leanrs's `rt/leanrs_rt/src/io/fs.rs` (`local/follow-native`).
fn walk_reads_non_link(path: &Path) -> bool {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::{Component, PathBuf};
    let mut dest = if path.is_absolute() {
        PathBuf::from("/")
    } else {
        match std::env::current_dir() {
            Ok(d) => d,
            Err(_) => return false,
        }
    };
    let comps = |p: &Path| -> Vec<OsString> {
        p.components()
            .filter_map(|c| match c {
                Component::Normal(n) => Some(n.to_os_string()),
                Component::ParentDir => Some(OsString::from("..")),
                _ => None,
            })
            .collect()
    };
    let mut rest: VecDeque<OsString> = comps(path).into();
    let mut links = 0;
    while let Some(c) = rest.pop_front() {
        if c == ".." {
            dest.pop();
            continue;
        }
        dest.push(&c);
        match std::fs::read_link(&dest) {
            Ok(target) => {
                links += 1;
                if links > 40 {
                    return false;
                }
                dest.pop();
                if target.is_absolute() {
                    dest = PathBuf::from("/");
                }
                for t in comps(&target).into_iter().rev() {
                    rest.push_front(t);
                }
            }
            Err(e) if e.raw_os_error() == Some(EINVAL) => return true,
            Err(_) => return false,
        }
    }
    false
}

/// `System.FilePath.readDir` (`lean_io_read_dir`, `opendir` then `readdir`):
/// `entry` gets each name in `readdir` order, `.` and `..` skipped; an
/// `opendir` failure names the directory; a failing `readdir` ends the listing
/// (Lean does not check it). The caller builds each `DirEntry` from the
/// directory (its argument) and the name.
pub fn read_dir(p: &[u8], mut entry: impl FnMut(&[u8])) -> Result<(), IoError> {
    // a descriptor is allocated: the context's handed-off streams end first
    super::effect_point();
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
/// path is `ERANGE`); the `errno` on failure. Never during a spawn that has
/// the process in its `cwd` (`process::with_cwd_read`, review RIO2-20).
fn getcwd_max() -> Result<Vec<u8>, i32> {
    match super::process::with_cwd_read(nix::unistd::getcwd) {
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
/// whole path. Never during a spawn that has the process in its `cwd`
/// (`process::with_cwd_change`).
pub fn set_current_dir(p: &[u8]) -> Result<(), IoError> {
    super::effect_point();
    let cut = p.iter().position(|&b| b == 0).unwrap_or(p.len());
    super::process::with_cwd_change(|| nix::unistd::chdir(os_path(&p[..cut]))).map_err(|e| {
        set_errno(e as i32);
        IoError::decode_io_error(e as i32, Some(p))
    })
}
