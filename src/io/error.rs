//! `IO.Error` as neutral data, and its construction from an `errno` or a
//! libuv error code, as Lean 4.34.0's `src/runtime/io.cpp` builds it.
//!
//! [`IoError`] mirrors Lean's `IO.Error` (`Init/System/IOError.lean`): its 19
//! constructors in Lean's order, with plain fields (file names and details as
//! `String`, the OS code as `u32`). Each translator converts it to its own
//! value, for example by calling the `lean_mk_io_error_*` constructor that
//! [`IoError::ctor_index`] names.
//!
//! Since Lean 4.34 both of io.cpp's decoders are one function,
//! `decode_uv_error_impl`:
//! - [`IoError::decode_io_error`] (`lean_decode_io_error`) maps the `errno`
//!   to libuv's code first ([`crt_to_uv`], which approximates the `errno`s
//!   libuv cannot represent), classifies that code, takes libuv's message for
//!   it ([`uv_strerror`]) and stores the `errno` as the OS code;
//! - [`IoError::decode_uv_error`] (`lean_decode_uv_error`), for the operations
//!   Lean implements with libuv, classifies the libuv code itself and stores
//!   its negation, the positive `errno` (`2` for `UV_ENOENT`).
//!
//! **LB-03** (`docs/lean-bugs.md`): io.cpp asserts a file name for
//! `UV_EINTR` and `UV_ENOENT` and dereferences a null one in release builds
//! (SIGSEGV). Here a nameless error of those classes gets the file name `""`
//! (`noFileOrDirectory "" 2 "no such file or directory"` for a `getcwd` in a
//! removed directory). The classes io.cpp asserts to have no file name ignore
//! one, as its release build does.
//!
//! The errno model: C's `errno` is observable in Lean through a stream's
//! sticky error indicator (a later `getLine` reports whatever `errno` holds
//! then). The crate's system calls go through rustix, which does not touch
//! libc's `errno`, so the crate keeps its own thread-local copy ([`errno`],
//! [`set_errno`]), which every failing call the crate models sets as the C
//! call would.
//!
//! Sources: lean2rr's `runtime/leanrt/src/fs.rs` (the 4.34 decoding: `crt_to_uv`,
//! the libuv message table, the class table, checked against native for every
//! `errno` 0..=140) and leanrs's `rt/leanrs_rt/src/io/error.rs` (the enum,
//! `embedded_nul`, the empty file name of LB-03).

use std::borrow::Cow;
use std::cell::Cell;

/// Lean's `IO.Error` (`Init/System/IOError.lean`): its 19 constructors in
/// Lean's order, with Lean's positional fields (`filename`, `osCode`,
/// `details`, or `msg` for `userError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoError {
    AlreadyExists(Option<String>, u32, String),
    OtherError(u32, String),
    ResourceBusy(u32, String),
    ResourceVanished(u32, String),
    UnsupportedOperation(u32, String),
    HardwareFault(u32, String),
    UnsatisfiedConstraints(u32, String),
    IllegalOperation(u32, String),
    ProtocolError(u32, String),
    TimeExpired(u32, String),
    Interrupted(String, u32, String),
    NoFileOrDirectory(String, u32, String),
    InvalidArgument(Option<String>, u32, String),
    PermissionDenied(Option<String>, u32, String),
    ResourceExhausted(Option<String>, u32, String),
    InappropriateType(Option<String>, u32, String),
    NoSuchThing(Option<String>, u32, String),
    UnexpectedEof,
    UserError(String),
}

// Linux `errno` values (`asm-generic/errno-base.h`, `asm-generic/errno.h`),
// the same on aarch64 and x86-64.
pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const ESRCH: i32 = 3;
pub const EINTR: i32 = 4;
pub const EIO: i32 = 5;
pub const ENXIO: i32 = 6;
pub const E2BIG: i32 = 7;
pub const ENOEXEC: i32 = 8;
pub const EBADF: i32 = 9;
pub const ECHILD: i32 = 10;
pub const EAGAIN: i32 = 11;
pub const EWOULDBLOCK: i32 = EAGAIN;
pub const ENOMEM: i32 = 12;
pub const EACCES: i32 = 13;
pub const EFAULT: i32 = 14;
pub const EBUSY: i32 = 16;
pub const EEXIST: i32 = 17;
pub const EXDEV: i32 = 18;
pub const ENODEV: i32 = 19;
pub const ENOTDIR: i32 = 20;
pub const EISDIR: i32 = 21;
pub const EINVAL: i32 = 22;
pub const ENFILE: i32 = 23;
pub const EMFILE: i32 = 24;
pub const ENOTTY: i32 = 25;
pub const ETXTBSY: i32 = 26;
pub const EFBIG: i32 = 27;
pub const ENOSPC: i32 = 28;
pub const ESPIPE: i32 = 29;
pub const EROFS: i32 = 30;
pub const EMLINK: i32 = 31;
pub const EPIPE: i32 = 32;
pub const EDOM: i32 = 33;
pub const ERANGE: i32 = 34;
pub const EDEADLK: i32 = 35;
pub const ENAMETOOLONG: i32 = 36;
pub const ENOLCK: i32 = 37;
pub const ENOSYS: i32 = 38;
pub const ENOTEMPTY: i32 = 39;
pub const ELOOP: i32 = 40;
pub const ENOMSG: i32 = 42;
pub const EIDRM: i32 = 43;
pub const ENOSTR: i32 = 60;
pub const ENODATA: i32 = 61;
pub const ETIME: i32 = 62;
pub const ENOSR: i32 = 63;
pub const ENOLINK: i32 = 67;
pub const EPROTO: i32 = 71;
pub const EBADMSG: i32 = 74;
pub const EILSEQ: i32 = 84;
pub const ENOTSOCK: i32 = 88;
pub const EDESTADDRREQ: i32 = 89;
pub const EMSGSIZE: i32 = 90;
pub const EPROTOTYPE: i32 = 91;
pub const ENOPROTOOPT: i32 = 92;
pub const EPROTONOSUPPORT: i32 = 93;
pub const EOPNOTSUPP: i32 = 95;
pub const EAFNOSUPPORT: i32 = 97;
pub const EADDRINUSE: i32 = 98;
pub const EADDRNOTAVAIL: i32 = 99;
pub const ENETDOWN: i32 = 100;
pub const ENETUNREACH: i32 = 101;
pub const ENETRESET: i32 = 102;
pub const ECONNABORTED: i32 = 103;
pub const ECONNRESET: i32 = 104;
pub const ENOBUFS: i32 = 105;
pub const EISCONN: i32 = 106;
pub const ENOTCONN: i32 = 107;
pub const ETIMEDOUT: i32 = 110;
pub const ECONNREFUSED: i32 = 111;
pub const EHOSTUNREACH: i32 = 113;
pub const EINPROGRESS: i32 = 115;

thread_local! {
    /// The crate's model of C's `errno` (see the module comment).
    static ERRNO: Cell<i32> = const { Cell::new(0) };
}

/// The calling thread's `errno` as the crate models it: the error of the last
/// failing call the crate made on this thread that sets C's `errno`.
#[inline]
pub fn errno() -> i32 {
    ERRNO.with(Cell::get)
}

/// Set the calling thread's modelled `errno` (a failing C call; `0` where
/// libuv clears it before a call).
#[inline]
pub fn set_errno(e: i32) {
    ERRNO.with(|c| c.set(e))
}

/// Lean 4.34's `lean_crt_to_uv_err` (io.cpp) on Linux: the libuv error code
/// (negative) of an `errno`. libuv's codes are the negated `errno`s there, so
/// an `errno` libuv names is itself negated; the ones libuv cannot represent
/// are approximated by the closest code it can (`EBADMSG` to `UV_EPROTO`, ...);
/// any other `errno` is negated too, which no case of `decode_uv_error_impl`
/// names (an `otherError`, "Unknown system error -e"). Lean 4.34 is compiled
/// against libuv headers of 1.45 or later (`ENODATA` and `ENOMSG` to
/// `UV_ENODATA`) and of 1.50 or later (`ENOEXEC` to `UV_ENOEXEC`).
pub fn crt_to_uv(e: i32) -> i32 {
    -match e {
        EBADMSG => EPROTO,
        ECHILD => ESRCH,
        EDEADLK => EBUSY,
        EDOM => EINVAL,
        EIDRM => EPIPE,
        EINPROGRESS => EISCONN,
        ENETRESET => ECONNRESET,
        ENOLCK => EAGAIN,
        ENOLINK => ECONNRESET,
        ENOSR => ENOBUFS,
        ENOSTR => EINVAL,
        ETIME => ETIMEDOUT,
        ENOMSG => ENODATA,
        _ => e,
    }
}

/// libuv's `uv_strerror` as linked into Lean 4.34 (libuv 1.48's
/// `UV_ERRNO_MAP` with Linux's codes): the message of a libuv code, or
/// `Unknown system error <code>` for a code it does not name (`UV_ENOEXEC`
/// among them: Lean classifies it, but the libuv it links has no message).
pub fn uv_strerror(code: i32) -> Cow<'static, str> {
    Cow::Borrowed(match -code {
        1 => "operation not permitted",
        2 => "no such file or directory",
        3 => "no such process",
        4 => "interrupted system call",
        5 => "i/o error",
        6 => "no such device or address",
        7 => "argument list too long",
        9 => "bad file descriptor",
        11 => "resource temporarily unavailable",
        12 => "not enough memory",
        13 => "permission denied",
        14 => "bad address in system call argument",
        16 => "resource busy or locked",
        17 => "file already exists",
        18 => "cross-device link not permitted",
        19 => "no such device",
        20 => "not a directory",
        21 => "illegal operation on a directory",
        22 => "invalid argument",
        23 => "file table overflow",
        24 => "too many open files",
        25 => "inappropriate ioctl for device",
        26 => "text file is busy",
        27 => "file too large",
        28 => "no space left on device",
        29 => "invalid seek",
        30 => "read-only file system",
        31 => "too many links",
        32 => "broken pipe",
        34 => "result too large",
        36 => "name too long",
        38 => "function not implemented",
        39 => "directory not empty",
        40 => "too many symbolic links encountered",
        49 => "protocol driver not attached",
        61 => "no data available",
        64 => "machine is not on the network",
        71 => "protocol error",
        75 => "value too large for defined data type",
        84 => "illegal byte sequence",
        88 => "socket operation on non-socket",
        89 => "destination address required",
        90 => "message too long",
        91 => "protocol wrong type for socket",
        92 => "protocol not available",
        93 => "protocol not supported",
        94 => "socket type not supported",
        95 => "operation not supported on socket",
        97 => "address family not supported",
        98 => "address already in use",
        99 => "address not available",
        100 => "network is down",
        101 => "network is unreachable",
        103 => "software caused connection abort",
        104 => "connection reset by peer",
        105 => "no buffer space available",
        106 => "socket is already connected",
        107 => "socket is not connected",
        108 => "cannot send after transport endpoint shutdown",
        110 => "connection timed out",
        111 => "connection refused",
        112 => "host is down",
        113 => "host is unreachable",
        114 => "connection already in progress",
        121 => "remote I/O error",
        125 => "operation canceled",
        3000 => "address family not supported",
        3001 => "temporary failure",
        3002 => "bad ai_flags value",
        3003 => "request canceled",
        3004 => "permanent failure",
        3005 => "ai_family not supported",
        3006 => "out of memory",
        3007 => "no address",
        3008 => "unknown node or service",
        3009 => "argument buffer overflow",
        3010 => "service not available for socket type",
        3011 => "socket type not supported",
        3013 => "invalid value for hints",
        3014 => "resolved protocol is unknown",
        4028 => "inappropriate file type or format",
        4080 => "invalid Unicode character",
        4094 => "unknown error",
        4095 => "end of file",
        _ => return Cow::Owned(format!("Unknown system error {code}")),
    })
}

/// A file name as Lean stores it: the bytes of a Lean string, so valid UTF-8
/// (decoded lossily if not).
fn name(f: &[u8]) -> String {
    String::from_utf8_lossy(f).into_owned()
}

impl IoError {
    /// `decode_uv_error_impl` (io.cpp): the class of the libuv code `uv`, its
    /// `uv_strerror` message, and `os_code` as the error code. A file name is
    /// kept by the classes that take one; `UV_EINTR` and `UV_ENOENT` without
    /// one get `""` (LB-03).
    pub fn decode_uv_error_impl(uv: i32, os_code: i32, fname: Option<&[u8]>) -> IoError {
        let details = uv_strerror(uv).into_owned();
        let code = os_code as u32;
        let file = || fname.map(name);
        let named = || fname.map(name).unwrap_or_default();
        match -uv {
            EINTR => IoError::Interrupted(named(), code, details),
            ELOOP | ENAMETOOLONG | EDESTADDRREQ | EBADF | EINVAL | EILSEQ | ENOTCONN | ENOTSOCK
            | ENOEXEC => IoError::InvalidArgument(file(), code, details),
            ENOENT => IoError::NoFileOrDirectory(named(), code, details),
            EACCES | EROFS | ECONNABORTED | EFBIG | EPERM => {
                IoError::PermissionDenied(file(), code, details)
            }
            EMFILE | ENFILE | ENOSPC | E2BIG | EAGAIN | EMLINK | EMSGSIZE | ENOBUFS | ENOMEM => {
                IoError::ResourceExhausted(file(), code, details)
            }
            EISDIR | ENOTDIR => IoError::InappropriateType(file(), code, details),
            ENXIO | EHOSTUNREACH | ENETUNREACH | ECONNREFUSED | ENODATA | ESRCH => {
                IoError::NoSuchThing(file(), code, details)
            }
            EEXIST | EISCONN => IoError::AlreadyExists(file(), code, details),
            EIO => IoError::HardwareFault(code, details),
            ENOTEMPTY => IoError::UnsatisfiedConstraints(code, details),
            ENOTTY => IoError::IllegalOperation(code, details),
            ECONNRESET | ENETDOWN | EPIPE => IoError::ResourceVanished(code, details),
            EPROTO | EPROTONOSUPPORT | EPROTOTYPE => IoError::ProtocolError(code, details),
            ETIMEDOUT => IoError::TimeExpired(code, details),
            EADDRINUSE | EBUSY | ETXTBSY => IoError::ResourceBusy(code, details),
            EADDRNOTAVAIL | EAFNOSUPPORT | ENODEV | ENOPROTOOPT | ENOSYS | EOPNOTSUPP | ERANGE
            | ESPIPE | EXDEV => IoError::UnsupportedOperation(code, details),
            _ => IoError::OtherError(code, details),
        }
    }

    /// `lean_decode_io_error(errnum, fname)`: an `errno` from a C library
    /// call, classified by its libuv code ([`crt_to_uv`]), with the `errno`
    /// as the error code.
    pub fn decode_io_error(errnum: i32, fname: Option<&[u8]>) -> IoError {
        IoError::decode_uv_error_impl(crt_to_uv(errnum), errnum, fname)
    }

    /// `lean_decode_uv_error(errnum, fname)`: a libuv error code (negative),
    /// classified as it is, with its negation as the error code.
    pub fn decode_uv_error(errnum: i32, fname: Option<&[u8]>) -> IoError {
        IoError::decode_uv_error_impl(errnum, errnum.wrapping_neg(), fname)
    }

    /// `decode_io_error` of the calling thread's modelled [`errno`].
    pub fn last_os_error(fname: Option<&[u8]>) -> IoError {
        IoError::decode_io_error(errno(), fname)
    }

    /// `mk_embedded_nul_error` (io.cpp): a path or name holding a NUL byte.
    pub fn embedded_nul(s: &[u8]) -> IoError {
        IoError::InvalidArgument(
            Some(name(s)),
            EINVAL as u32,
            "string contains NUL bytes".to_owned(),
        )
    }

    /// `mk_file_not_found_error` (io.cpp): `realPath`'s error, whatever
    /// `realpath` reported.
    pub fn file_not_found(fname: &[u8]) -> IoError {
        IoError::NoFileOrDirectory(name(fname), ENOENT as u32, String::new())
    }

    /// `io_result_mk_error(msg)` (io.cpp): `IO.userError msg`.
    pub fn user_error(msg: &str) -> IoError {
        IoError::UserError(msg.to_owned())
    }

    /// The constructor's index in Lean's declaration order (`alreadyExists`
    /// 0, ..., `unexpectedEof` 17, `userError` 18).
    pub fn ctor_index(&self) -> u8 {
        match self {
            IoError::AlreadyExists(..) => 0,
            IoError::OtherError(..) => 1,
            IoError::ResourceBusy(..) => 2,
            IoError::ResourceVanished(..) => 3,
            IoError::UnsupportedOperation(..) => 4,
            IoError::HardwareFault(..) => 5,
            IoError::UnsatisfiedConstraints(..) => 6,
            IoError::IllegalOperation(..) => 7,
            IoError::ProtocolError(..) => 8,
            IoError::TimeExpired(..) => 9,
            IoError::Interrupted(..) => 10,
            IoError::NoFileOrDirectory(..) => 11,
            IoError::InvalidArgument(..) => 12,
            IoError::PermissionDenied(..) => 13,
            IoError::ResourceExhausted(..) => 14,
            IoError::InappropriateType(..) => 15,
            IoError::NoSuchThing(..) => 16,
            IoError::UnexpectedEof => 17,
            IoError::UserError(..) => 18,
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
