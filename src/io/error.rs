//! `IO.Error` as neutral data, and its construction from an `errno` or a
//! libuv error code, as Lean 4.34.0's `src/runtime/io.cpp` builds it.
//!
//! [`IoError`] mirrors Lean's `IO.Error` (`Init/System/IOError.lean`): its 19
//! constructors in Lean's order, with plain fields (file names and details as
//! `String`, the OS code as `u32`). Each translator converts it to its own
//! value, for example by calling the `lean_mk_io_error_*` builder that
//! [`IoError::builder_index`] names, or keeps it with its own string type
//! (`IoError<S>`, [`IoError::map_str`]). The accessors ([`IoError::os_code`],
//! [`IoError::file_name`], [`IoError::details`], [`IoError::ctor_index`])
//! read the fields without a `match` in the glue.
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
//! **LB-47** (`docs/lean-bugs.md`): `EBADMSG` (74, a file system's checksum
//! failure, `EFSBADCRC`) is `inappropriateType`, as `IO.Error`'s
//! documentation lists it (`Init/System/IOError.lean` 125) and io.cpp's own
//! comment places it (299), on both paths. Natively the C library's path
//! makes it `protocolError` (through `UV_EPROTO`) and the libuv path
//! `otherError` (libuv has no code for it). The details stay native's:
//! `protocol error`, and `Unknown system error -74`.
//!
//! The errno model: natively, C's `errno` is observable in Lean through a
//! stream's sticky error indicator (a later `getLine` reports whatever
//! `errno` holds then). lean-runtime's `getLine` reports only its own error
//! (LB-41, `docs/lean-bugs.md`), and that was the only channel: no
//! Lean-visible channel for a stale `errno` remains, and every error a call
//! reports carries the `errno` that call set. The crate's system calls go
//! through rustix, which does not touch libc's `errno`, so the crate keeps
//! its own thread-local copy ([`errno`], [`set_errno`]), which every failing
//! call the crate models sets as the C call would.
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
///
/// `S` is the string type of the file names and details. The crate's
/// functions return `IoError<String>` (the default, `IoError`). A glue may
/// keep its own string type (leanrs's `Str`): `e.map_str(Str::from)`, or `?`
/// through [`IoText`]; a type alias keeps the variant names in its code
/// (`type IoError = lean_runtime::io::IoError<Str>;`).
///
/// Where the string type is not fixed by the context, a variant without a
/// string needs it named: `IoError::<String>::UnexpectedEof.ctor_index()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IoError<S = String> {
    AlreadyExists(Option<S>, u32, S),
    OtherError(u32, S),
    ResourceBusy(u32, S),
    ResourceVanished(u32, S),
    UnsupportedOperation(u32, S),
    HardwareFault(u32, S),
    UnsatisfiedConstraints(u32, S),
    IllegalOperation(u32, S),
    ProtocolError(u32, S),
    TimeExpired(u32, S),
    Interrupted(S, u32, S),
    NoFileOrDirectory(S, u32, S),
    InvalidArgument(Option<S>, u32, S),
    PermissionDenied(Option<S>, u32, S),
    ResourceExhausted(Option<S>, u32, S),
    InappropriateType(Option<S>, u32, S),
    NoSuchThing(Option<S>, u32, S),
    UnexpectedEof,
    UserError(S),
}

/// A glue's string type for [`IoError<S>`]: made from the crate's `String`.
/// With it, `?` converts the crate's `IoError` (`From<IoError<String>> for
/// IoError<S>`, by [`IoError::map_str`]). `String` itself does not
/// implement it: an `IoError<String>` needs no conversion.
pub trait IoText: Sized {
    /// The glue's string of a file name or a details text.
    fn from_io_text(s: String) -> Self;
}

impl<S: IoText> From<IoError<String>> for IoError<S> {
    fn from(e: IoError<String>) -> IoError<S> {
        e.map_str(S::from_io_text)
    }
}

/// The `lean_mk_io_error_*` builders io.cpp calls (and `lean_mk_io_user_error`
/// for `userError`), each at its [`IoError::builder_index`]. Lean defines them
/// in `Init/System/IOError.lean` (`@[export]`). The `_file` builders take the
/// file name; the others drop it. `lean_mk_io_error_eof` (for
/// `unexpectedEof`) is not here: no io function returns that error.
pub const IO_ERROR_BUILDERS: [&str; 24] = [
    "lean_mk_io_error_other_error",
    "lean_mk_io_error_interrupted",
    "lean_mk_io_error_invalid_argument",
    "lean_mk_io_error_invalid_argument_file",
    "lean_mk_io_error_no_file_or_directory",
    "lean_mk_io_error_permission_denied",
    "lean_mk_io_error_permission_denied_file",
    "lean_mk_io_error_resource_exhausted",
    "lean_mk_io_error_resource_exhausted_file",
    "lean_mk_io_error_inappropriate_type",
    "lean_mk_io_error_inappropriate_type_file",
    "lean_mk_io_error_no_such_thing",
    "lean_mk_io_error_no_such_thing_file",
    "lean_mk_io_error_already_exists",
    "lean_mk_io_error_already_exists_file",
    "lean_mk_io_error_hardware_fault",
    "lean_mk_io_error_unsatisfied_constraints",
    "lean_mk_io_error_illegal_operation",
    "lean_mk_io_error_resource_vanished",
    "lean_mk_io_error_protocol_error",
    "lean_mk_io_error_time_expired",
    "lean_mk_io_error_resource_busy",
    "lean_mk_io_error_unsupported_operation",
    "lean_mk_io_user_error",
];

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
/// are approximated by the closest code it can (`EBADMSG` to `UV_EPROTO`, ...,
/// which gives `EBADMSG` its message; its class is LB-47's);
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
    match uv_strerror_named(code) {
        Some(m) => Cow::Borrowed(m),
        None => Cow::Owned(format!("Unknown system error {code}")),
    }
}

/// [`uv_strerror`] of a code libuv names, `None` for the others: without an
/// allocation (the startup descriptors' failure in an ELF constructor,
/// AR-36).
pub(crate) fn uv_strerror_named(code: i32) -> Option<&'static str> {
    Some(match -code {
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
        _ => return None,
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
    /// one get `""` (LB-03). `EBADMSG` is `inappropriateType` from either
    /// decoder (LB-47).
    pub fn decode_uv_error_impl(uv: i32, os_code: i32, fname: Option<&[u8]>) -> IoError {
        let details = uv_strerror(uv).into_owned();
        let code = os_code as u32;
        let file = || fname.map(name);
        let named = || fname.map(name).unwrap_or_default();
        // LB-47: `EBADMSG`, from the C library (classified as `UV_EPROTO`)
        // or from libuv (its own negation, which no case names)
        if os_code == EBADMSG && matches!(-uv, EPROTO | EBADMSG) {
            return IoError::InappropriateType(file(), code, details);
        }
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
}

impl<S> IoError<S> {
    /// The same error with each string (file name, details, `userError`'s
    /// message) passed through `f`, in field order: a glue's own string type
    /// (leanrs: `e.map_str(Str::from)`).
    pub fn map_str<T>(self, mut f: impl FnMut(S) -> T) -> IoError<T> {
        use IoError as E;
        match self {
            E::AlreadyExists(n, c, d) => E::AlreadyExists(n.map(&mut f), c, f(d)),
            E::OtherError(c, d) => E::OtherError(c, f(d)),
            E::ResourceBusy(c, d) => E::ResourceBusy(c, f(d)),
            E::ResourceVanished(c, d) => E::ResourceVanished(c, f(d)),
            E::UnsupportedOperation(c, d) => E::UnsupportedOperation(c, f(d)),
            E::HardwareFault(c, d) => E::HardwareFault(c, f(d)),
            E::UnsatisfiedConstraints(c, d) => E::UnsatisfiedConstraints(c, f(d)),
            E::IllegalOperation(c, d) => E::IllegalOperation(c, f(d)),
            E::ProtocolError(c, d) => E::ProtocolError(c, f(d)),
            E::TimeExpired(c, d) => E::TimeExpired(c, f(d)),
            E::Interrupted(n, c, d) => {
                let n = f(n);
                E::Interrupted(n, c, f(d))
            }
            E::NoFileOrDirectory(n, c, d) => {
                let n = f(n);
                E::NoFileOrDirectory(n, c, f(d))
            }
            E::InvalidArgument(n, c, d) => E::InvalidArgument(n.map(&mut f), c, f(d)),
            E::PermissionDenied(n, c, d) => E::PermissionDenied(n.map(&mut f), c, f(d)),
            E::ResourceExhausted(n, c, d) => E::ResourceExhausted(n.map(&mut f), c, f(d)),
            E::InappropriateType(n, c, d) => E::InappropriateType(n.map(&mut f), c, f(d)),
            E::NoSuchThing(n, c, d) => E::NoSuchThing(n.map(&mut f), c, f(d)),
            E::UnexpectedEof => E::UnexpectedEof,
            E::UserError(m) => E::UserError(f(m)),
        }
    }

    /// The `osCode` field, or `None` for `unexpectedEof` and `userError`,
    /// which have none.
    pub fn os_code(&self) -> Option<u32> {
        use IoError as E;
        match self {
            E::AlreadyExists(_, c, _)
            | E::Interrupted(_, c, _)
            | E::NoFileOrDirectory(_, c, _)
            | E::InvalidArgument(_, c, _)
            | E::PermissionDenied(_, c, _)
            | E::ResourceExhausted(_, c, _)
            | E::InappropriateType(_, c, _)
            | E::NoSuchThing(_, c, _)
            | E::OtherError(c, _)
            | E::ResourceBusy(c, _)
            | E::ResourceVanished(c, _)
            | E::UnsupportedOperation(c, _)
            | E::HardwareFault(c, _)
            | E::UnsatisfiedConstraints(c, _)
            | E::IllegalOperation(c, _)
            | E::ProtocolError(c, _)
            | E::TimeExpired(c, _) => Some(*c),
            E::UnexpectedEof | E::UserError(_) => None,
        }
    }

    /// The `filename` field: always there for `interrupted` and
    /// `noFileOrDirectory`, optional for the seven classes with an
    /// `Option String`, `None` for the others.
    pub fn file_name(&self) -> Option<&S> {
        use IoError as E;
        match self {
            E::Interrupted(n, _, _) | E::NoFileOrDirectory(n, _, _) => Some(n),
            E::AlreadyExists(n, _, _)
            | E::InvalidArgument(n, _, _)
            | E::PermissionDenied(n, _, _)
            | E::ResourceExhausted(n, _, _)
            | E::InappropriateType(n, _, _)
            | E::NoSuchThing(n, _, _) => n.as_ref(),
            _ => None,
        }
    }

    /// The `details` field (libuv's message, or Lean's own text), or
    /// `userError`'s message; `None` for `unexpectedEof`.
    pub fn details(&self) -> Option<&S> {
        use IoError as E;
        match self {
            E::AlreadyExists(_, _, d)
            | E::Interrupted(_, _, d)
            | E::NoFileOrDirectory(_, _, d)
            | E::InvalidArgument(_, _, d)
            | E::PermissionDenied(_, _, d)
            | E::ResourceExhausted(_, _, d)
            | E::InappropriateType(_, _, d)
            | E::NoSuchThing(_, _, d)
            | E::OtherError(_, d)
            | E::ResourceBusy(_, d)
            | E::ResourceVanished(_, d)
            | E::UnsupportedOperation(_, d)
            | E::HardwareFault(_, d)
            | E::UnsatisfiedConstraints(_, d)
            | E::IllegalOperation(_, d)
            | E::ProtocolError(_, d)
            | E::TimeExpired(_, d)
            | E::UserError(d) => Some(d),
            E::UnexpectedEof => None,
        }
    }

    /// The `lean_mk_io_error_*` builder that makes this error, as its index in
    /// [`IO_ERROR_BUILDERS`] (lean2rr's numbering): the class, and for the
    /// seven classes with an optional file name whether it has one (`_file`,
    /// one more). `userError` is 23. `None` for `unexpectedEof`, which no io
    /// function returns (Lean's builder for it, `lean_mk_io_error_eof`, has
    /// no index here).
    pub fn builder_index(&self) -> Option<u8> {
        use IoError as E;
        let file = |n: &Option<S>, i: u8| if n.is_some() { i + 1 } else { i };
        Some(match self {
            E::OtherError(..) => 0,
            E::Interrupted(..) => 1,
            E::InvalidArgument(n, ..) => file(n, 2),
            E::NoFileOrDirectory(..) => 4,
            E::PermissionDenied(n, ..) => file(n, 5),
            E::ResourceExhausted(n, ..) => file(n, 7),
            E::InappropriateType(n, ..) => file(n, 9),
            E::NoSuchThing(n, ..) => file(n, 11),
            E::AlreadyExists(n, ..) => file(n, 13),
            E::HardwareFault(..) => 15,
            E::UnsatisfiedConstraints(..) => 16,
            E::IllegalOperation(..) => 17,
            E::ResourceVanished(..) => 18,
            E::ProtocolError(..) => 19,
            E::TimeExpired(..) => 20,
            E::ResourceBusy(..) => 21,
            E::UnsupportedOperation(..) => 22,
            E::UserError(..) => 23,
            E::UnexpectedEof => return None,
        })
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
