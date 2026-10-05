//! `IO.FS.createTempFile` and `IO.FS.createTempDir` (Lean 4.34.0's `io.cpp`,
//! `lean_io_create_tempfile` and `lean_io_create_tempdir`): the directory is
//! libuv's `uv_os_tmpdir` ([`super::uvsys`]), the name `tmp.XXXXXXXX` with
//! its last six characters chosen as glibc's `__gen_tempname` chooses them,
//! the file created as `uv_fs_mkstemp` creates it (`mkostemp` with
//! `O_CLOEXEC`: `O_RDWR | O_CREAT | O_EXCL`, mode 0600) and the directory as
//! `uv_fs_mkdtemp` (`mkdtemp`: mode 0700). Errors are `decode_uv_error(e,
//! nullptr)`; an empty directory is `UV_ENOENT` with the file name `""`.
//!
//! Two native failures are replaced by the system's answer:
//! - a missing directory (`ENOENT` without a file name) crashes natively
//!   (LB-03); here it is `noFileOrDirectory ""`, case `temp/temp_missing_dir`;
//! - a directory of 4083 to 4095 bytes fails Lean's
//!   `lean_always_assert(PATH_MAX >= ...)` natively (status 134, LB-16);
//!   here the creation is tried and the kernel's `ENAMETOOLONG` reported,
//!   cases `temp/temp_long_dir`, `temp_long_file`, `temp_long_dir_4095`.
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/temp.rs` (probe
//! `validate/io/temp_files`) and lean2rr's `runtime/leanrt/src/fs.rs`
//! (`temp_template`, `create_temp_file`, `create_temp_dir`).

use std::collections::hash_map::RandomState;
use std::ffi::OsStr;
use std::fs::{DirBuilder, OpenOptions};
use std::hash::{BuildHasher, Hasher};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use super::error::{set_errno, IoError, EEXIST, ENOENT};
use super::handle::{FsMode, Handle};
use super::{uvsys, ByteSink};

/// glibc's `__gen_tempname` tries this many names (62³) before giving up
/// with `EEXIST`.
const ATTEMPTS: u32 = 62 * 62 * 62;
const LETTERS: &[u8; 62] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// `decode_uv_error(-e, nullptr)` of the creation's failure, with the
/// modelled `errno` set to `e`.
fn uv(e: i32) -> IoError {
    set_errno(e);
    IoError::decode_uv_error(-e, None)
}

/// The template: `uv_os_tmpdir`'s directory, a `/` unless it ends in one,
/// then `tmp.XXXXXXXX`. An empty directory is `UV_ENOENT` with the file
/// name `""`. These errors are libuv's and Lean's own: `errno` keeps its
/// value.
fn template() -> Result<Vec<u8>, IoError> {
    let mut dir = uvsys::uv_os_tmpdir().map_err(|e| IoError::decode_uv_error(-e, None))?;
    if dir.is_empty() {
        return Err(IoError::decode_uv_error(-ENOENT, Some(b"")));
    }
    if dir.last() != Some(&b'/') {
        dir.push(b'/');
    }
    dir.extend_from_slice(b"tmp.XXXXXXXX");
    Ok(dir)
}

/// glibc's `__gen_tempname`: the template's last six bytes replaced by
/// letters and digits until `create` does not fail with `EEXIST`. libuv's
/// `uv__fs_work` clears `errno` before the call and glibc restores it after
/// the names that existed, so a success leaves `errno` at 0.
fn gen_name<T>(
    mut path: Vec<u8>,
    create: impl Fn(&OsStr) -> std::io::Result<T>,
) -> Result<(T, Vec<u8>), IoError> {
    let n = path.len();
    set_errno(0);
    let seed = RandomState::new();
    for attempt in 0..ATTEMPTS {
        let mut h = seed.build_hasher();
        h.write_u32(attempt);
        let mut bits = h.finish();
        for b in &mut path[n - 6..] {
            *b = LETTERS[(bits % 62) as usize];
            bits /= 62;
        }
        // threads mode: never looked up in a fallback spawn's `cwd` (a
        // relative `TMPDIR`; review RT1-04)
        let made = super::process::with_path_lookup(|| create(OsStr::from_bytes(&path)));
        match made {
            Ok(t) => return Ok((t, path)),
            Err(e) if e.raw_os_error() == Some(EEXIST) => {}
            Err(e) => return Err(uv(e.raw_os_error().unwrap_or(ENOENT))),
        }
    }
    Err(uv(EEXIST))
}

/// `IO.FS.createTempFile`: a new file, opened `fdopen(fd, "r+")`; its path
/// is appended to `path`.
pub fn create_temp_file<S: ByteSink + ?Sized>(path: &mut S) -> Result<Handle, IoError> {
    super::effect_point();
    let (file, p) = gen_name(template()?, |p| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(p)
    })?;
    path.extend_from_slice(&p);
    Ok(Handle::fdopen(file.into(), FsMode::ReadWrite))
}

/// `IO.FS.createTempDir`: a new directory; its path is appended to `path`.
pub fn create_temp_dir<S: ByteSink + ?Sized>(path: &mut S) -> Result<(), IoError> {
    super::effect_point();
    let ((), p) = gen_name(template()?, |p| DirBuilder::new().mode(0o700).create(p))?;
    path.extend_from_slice(&p);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// lean (temp_files): the name is `tmp.XX` and six letters or digits in
    /// the temporary directory, the file empty, read-write, mode 0600; the
    /// directory mode 0700 (leanrs's `io_create_temp_file`, `_dir`).
    #[cfg(not(miri))]
    #[test]
    fn file_and_dir() {
        let mut p = Vec::new();
        let h = create_temp_file(&mut p).unwrap();
        let path = String::from_utf8(p).unwrap();
        let name = path.rsplit('/').next().unwrap();
        assert!(name.starts_with("tmp.XX") && name.len() == 12, "{path}");
        assert!(name[6..].bytes().all(|b| b.is_ascii_alphanumeric()));
        let md = std::fs::metadata(&path).unwrap();
        assert_eq!((md.len(), md.permissions().mode() & 0o077), (0, 0));
        assert_eq!(md.permissions().mode() & 0o600, 0o600);
        h.put_str(b"hello temp\n").unwrap();
        h.rewind().unwrap();
        let mut l = Vec::new();
        h.get_line(&mut l).unwrap();
        assert_eq!(l, b"hello temp\n");
        std::fs::remove_file(&path).unwrap();

        let mut p = Vec::new();
        create_temp_dir(&mut p).unwrap();
        let path = String::from_utf8(p).unwrap();
        assert!(path.rsplit('/').next().unwrap().starts_with("tmp.XX"));
        let md = std::fs::metadata(&path).unwrap();
        assert!(md.is_dir() && md.permissions().mode() & 0o077 == 0);
        std::fs::remove_dir(&path).unwrap();
    }
}
