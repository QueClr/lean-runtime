//! The process environment as C's `environ` holds it: every entry, in order,
//! including the ones `std` cannot list or name (`=x`, an entry without `=`,
//! a name given twice).
//!
//! Two of Lean's operations read `environ` itself rather than one variable:
//! `Std.Internal.UV.System.osEnviron` (libuv's `uv_os_environ`, which lists
//! every entry holding a `=`, split at its first `=`) and `IO.Process.spawn`,
//! whose forked child starts from the parent's `environ` and applies `setenv`
//! and `unsetenv` to it. `std` offers no raw view of `environ`
//! (`std::env::vars_os` skips `=x` and splits `=a=b` after its first byte), so
//! the crate keeps this copy: read from `/proc/self/environ` (the entries the
//! process started with) at its first use, before any change, and changed as
//! glibc's `setenv(name, value, 1)` (the first entry of the name replaced in
//! place, else one appended) and `unsetenv` (every entry of the name removed)
//! change `environ`. Without `/proc` it starts from `std::env::vars_os`.
//!
//! Every change to the environment goes through [`set`] and [`unset`], which
//! change the C environment (`std::env::set_var`, `remove_var`, so
//! `IO.getEnv` sees them) and this copy together. Nothing else changes it: a
//! translated program links no user C (the owner's rule: Lean programs over
//! `Init` and `Std`), so no C code calls `setenv` behind the copy's back.
//!
//! One difference remains, outside the programs both translators target: in
//! a secure-execution process (set-user-ID, set-group-ID or file
//! capabilities, `AT_SECURE`), glibc removes the unsafe variables (`LD_*`
//! and the like) from `environ` at startup, while `/proc/self/environ` keeps
//! the strings the process started with, so the copy would still hold them.
//!
//! Source: leanrs's `rt/leanrs_rt/src/io/env.rs` (`ENVIRON`, `env_set`,
//! `env_unset`, `environ_entries`; probe `validate/io/proc_inherit`).

use std::ffi::{CString, OsStr};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::sync::{Arc, Mutex, PoisonError};

/// The entries, and the same as the C strings of a child's `envp`, built at
/// the first spawn after a change and shared by the spawns until the next.
struct Environ {
    entries: Vec<Vec<u8>>,
    envp: Option<Arc<[CString]>>,
}

static ENVIRON: Mutex<Option<Environ>> = Mutex::new(None);

fn initial() -> Vec<Vec<u8>> {
    match std::fs::read("/proc/self/environ") {
        Ok(b) => from_proc(&b),
        Err(_) => std::env::vars_os()
            .map(|(k, v)| {
                let mut e = k.into_vec();
                e.push(b'=');
                e.extend_from_slice(v.as_bytes());
                e
            })
            .collect(),
    }
}

/// The entries of `/proc/self/environ`'s bytes: each ends with a NUL byte,
/// so only the piece after the last one is no entry; an empty entry (an
/// `envp` may hold `""`) is kept.
fn from_proc(b: &[u8]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = b.split(|&c| c == 0).map(<[u8]>::to_vec).collect();
    if v.last().is_some_and(Vec::is_empty) {
        v.pop();
    }
    v
}

fn with_state<R>(f: impl FnOnce(&mut Environ) -> R) -> R {
    let mut g = ENVIRON.lock().unwrap_or_else(PoisonError::into_inner);
    f(g.get_or_insert_with(|| Environ {
        entries: initial(),
        envp: None,
    }))
}

fn with<R>(f: impl FnOnce(&Vec<Vec<u8>>) -> R) -> R {
    with_state(|st| f(&st.entries))
}

/// A change to the entries; the cached `envp` goes.
fn change(f: impl FnOnce(&mut Vec<Vec<u8>>)) {
    with_state(|st| {
        st.envp = None;
        f(&mut st.entries)
    })
}

/// The entries as C strings, a child's `envp` when it inherits the
/// environment unchanged (an entry holds no NUL byte: `/proc` separates them
/// with NULs, and [`set`] refuses one).
pub(crate) fn envp() -> Arc<[CString]> {
    with_state(|st| {
        let entries = &st.entries;
        st.envp
            .get_or_insert_with(|| {
                entries
                    .iter()
                    .filter_map(|e| CString::new(e.as_slice()).ok())
                    .collect()
            })
            .clone()
    })
}

/// Whether `entry` is an entry of `name` (`name=...`), as glibc's `getenv`,
/// `setenv` and `unsetenv` match them.
#[inline]
fn is_entry_of(entry: &[u8], name: &[u8]) -> bool {
    entry.len() > name.len() && entry.starts_with(name) && entry[name.len()] == b'='
}

/// glibc's `setenv(name, value, 1)` on a list of entries: the first entry of
/// `name` replaced in place, else `name=value` appended. `name` is one
/// `setenv` accepts (not empty, no `=`).
pub(crate) fn apply_set(env: &mut Vec<Vec<u8>>, name: &[u8], value: &[u8]) {
    let mut entry = Vec::with_capacity(name.len() + 1 + value.len());
    entry.extend_from_slice(name);
    entry.push(b'=');
    entry.extend_from_slice(value);
    match env.iter_mut().find(|e| is_entry_of(e, name)) {
        Some(e) => *e = entry,
        None => env.push(entry),
    }
}

/// glibc's `unsetenv(name)` on a list of entries: every entry of `name`
/// removed.
pub(crate) fn apply_unset(env: &mut Vec<Vec<u8>>, name: &[u8]) {
    env.retain(|e| !is_entry_of(e, name));
}

/// A name `setenv` and `unsetenv` accept: not empty, no `=` (they fail with
/// `EINVAL` otherwise). The caller has cut it at its first NUL byte or
/// refused one holding a NUL byte.
#[inline]
pub(crate) fn name_ok(name: &[u8]) -> bool {
    !name.is_empty() && !name.contains(&b'=')
}

/// The entries of `environ`, in order (a copy, taken under the lock).
pub fn entries() -> Vec<Vec<u8>> {
    with(|env| env.clone())
}

/// glibc's `getenv(name)` on `environ`: the value of the first entry
/// `name=value`; `None` for an empty name. A name holding `=` matches the
/// start of an entry, as glibc's does.
pub fn getenv(name: &[u8]) -> Option<Vec<u8>> {
    if name.is_empty() {
        return None;
    }
    with(|env| {
        env.iter()
            .find(|e| is_entry_of(e, name))
            .map(|e| e[name.len() + 1..].to_vec())
    })
}

/// `setenv(name, value, 1)`, on the C environment and on the copy. `name` is
/// one `setenv` accepts ([`name_ok`]) and neither holds a NUL byte.
pub fn set(name: &[u8], value: &[u8]) {
    super::effect_point();
    debug_assert!(name_ok(name) && !name.contains(&0) && !value.contains(&0));
    change(|env| {
        std::env::set_var(OsStr::from_bytes(name), OsStr::from_bytes(value));
        apply_set(env, name, value);
    })
}

/// `unsetenv(name)`, on the C environment and on the copy. `name` is one
/// `unsetenv` accepts ([`name_ok`]) and holds no NUL byte.
pub fn unset(name: &[u8]) {
    super::effect_point();
    debug_assert!(name_ok(name) && !name.contains(&0));
    change(|env| {
        std::env::remove_var(OsStr::from_bytes(name));
        apply_unset(env, name);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(v: &[&str]) -> Vec<Vec<u8>> {
        v.iter().map(|s| s.as_bytes().to_vec()).collect()
    }

    /// glibc's `setenv` replaces the first entry of the name in place and
    /// leaves a duplicate; `unsetenv` removes every entry; entries no name
    /// can reach stay (probe `validate/io/proc_inherit`: `changed` and
    /// `plain`).
    #[test]
    fn set_and_unset_as_glibc() {
        let mut env = list(&["=x", "A=1", "=a=b", "B=2", "=", "D=4", "B=dup", "noequals"]);
        apply_unset(&mut env, b"A");
        apply_set(&mut env, b"B", b"9");
        assert_eq!(
            env,
            list(&["=x", "=a=b", "B=9", "=", "D=4", "B=dup", "noequals"])
        );
        apply_set(&mut env, b"NEW", b"");
        apply_unset(&mut env, b"B");
        assert_eq!(env, list(&["=x", "=a=b", "=", "D=4", "noequals", "NEW="]));
        // a name matches its own entries only, never an entry without `=`
        apply_unset(&mut env, b"NE");
        apply_unset(&mut env, b"noequals");
        assert_eq!(env.len(), 6);
    }

    /// `/proc/self/environ`'s pieces: the trailing one is dropped, an empty
    /// entry kept (review RIO2-07).
    #[test]
    fn proc_pieces() {
        assert_eq!(from_proc(b"A=1\0B=2\0"), list(&["A=1", "B=2"]));
        assert_eq!(from_proc(b"A=1\0\0B=2\0"), list(&["A=1", "", "B=2"]));
        assert_eq!(from_proc(b""), Vec::<Vec<u8>>::new());
        assert_eq!(from_proc(b"\0"), list(&[""]));
    }

    #[test]
    fn names_setenv_accepts() {
        assert!(name_ok(b"A"));
        assert!(!name_ok(b""));
        assert!(!name_ok(b"A=B"));
    }
}
