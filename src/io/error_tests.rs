//! The error decoding against native Lean 4.34.0, for every `errno` 0..=140:
//! what `lean_decode_io_error(e, "f")` and `lean_decode_uv_error(-e, "f")` of
//! Lean's runtime build return (the constructor as lean2rr's kind number, the
//! stored code, the details). The table was printed by a native Lean program
//! calling the two C functions (aarch64 Linux); it comes from lean2rr's
//! `runtime/leanrt/src/fs_tests.rs`.

use super::*;

/// The builder number of an error (`IoError::builder_index`, lean2rr's kind
/// number), 24 for `unexpectedEof`, which has none.
fn kind(e: &IoError) -> u32 {
    e.builder_index().map_or(24, u32::from)
}

/// The code and details through the accessors (0 and `""` where there are
/// none).
fn code_details(e: &IoError) -> (u32, &str) {
    (
        e.os_code().unwrap_or(0),
        e.details().map_or("", String::as_str),
    )
}

/// (errno, io kind, io code, io details, uv kind, uv code, uv details)
#[rustfmt::skip]
const NATIVE: &[(i32, u32, u32, &str, u32, u32, &str)] = &[
    (0, 0, 0, "Unknown system error 0", 0, 0, "Unknown system error 0"),
    (1, 6, 1, "operation not permitted", 6, 1, "operation not permitted"),
    (2, 4, 2, "no such file or directory", 4, 2, "no such file or directory"),
    (3, 12, 3, "no such process", 12, 3, "no such process"),
    (4, 1, 4, "interrupted system call", 1, 4, "interrupted system call"),
    (5, 15, 5, "i/o error", 15, 5, "i/o error"),
    (6, 12, 6, "no such device or address", 12, 6, "no such device or address"),
    (7, 8, 7, "argument list too long", 8, 7, "argument list too long"),
    (8, 3, 8, "Unknown system error -8", 3, 8, "Unknown system error -8"),
    (9, 3, 9, "bad file descriptor", 3, 9, "bad file descriptor"),
    (10, 12, 10, "no such process", 0, 10, "Unknown system error -10"),
    (11, 8, 11, "resource temporarily unavailable", 8, 11, "resource temporarily unavailable"),
    (12, 8, 12, "not enough memory", 8, 12, "not enough memory"),
    (13, 6, 13, "permission denied", 6, 13, "permission denied"),
    (14, 0, 14, "bad address in system call argument", 0, 14, "bad address in system call argument"),
    (15, 0, 15, "Unknown system error -15", 0, 15, "Unknown system error -15"),
    (16, 21, 16, "resource busy or locked", 21, 16, "resource busy or locked"),
    (17, 14, 17, "file already exists", 14, 17, "file already exists"),
    (18, 22, 18, "cross-device link not permitted", 22, 18, "cross-device link not permitted"),
    (19, 22, 19, "no such device", 22, 19, "no such device"),
    (20, 10, 20, "not a directory", 10, 20, "not a directory"),
    (21, 10, 21, "illegal operation on a directory", 10, 21, "illegal operation on a directory"),
    (22, 3, 22, "invalid argument", 3, 22, "invalid argument"),
    (23, 8, 23, "file table overflow", 8, 23, "file table overflow"),
    (24, 8, 24, "too many open files", 8, 24, "too many open files"),
    (25, 17, 25, "inappropriate ioctl for device", 17, 25, "inappropriate ioctl for device"),
    (26, 21, 26, "text file is busy", 21, 26, "text file is busy"),
    (27, 6, 27, "file too large", 6, 27, "file too large"),
    (28, 8, 28, "no space left on device", 8, 28, "no space left on device"),
    (29, 22, 29, "invalid seek", 22, 29, "invalid seek"),
    (30, 6, 30, "read-only file system", 6, 30, "read-only file system"),
    (31, 8, 31, "too many links", 8, 31, "too many links"),
    (32, 18, 32, "broken pipe", 18, 32, "broken pipe"),
    (33, 3, 33, "invalid argument", 0, 33, "Unknown system error -33"),
    (34, 22, 34, "result too large", 22, 34, "result too large"),
    (35, 21, 35, "resource busy or locked", 0, 35, "Unknown system error -35"),
    (36, 3, 36, "name too long", 3, 36, "name too long"),
    (37, 8, 37, "resource temporarily unavailable", 0, 37, "Unknown system error -37"),
    (38, 22, 38, "function not implemented", 22, 38, "function not implemented"),
    (39, 16, 39, "directory not empty", 16, 39, "directory not empty"),
    (40, 3, 40, "too many symbolic links encountered", 3, 40, "too many symbolic links encountered"),
    (41, 0, 41, "Unknown system error -41", 0, 41, "Unknown system error -41"),
    (42, 12, 42, "no data available", 0, 42, "Unknown system error -42"),
    (43, 18, 43, "broken pipe", 0, 43, "Unknown system error -43"),
    (44, 0, 44, "Unknown system error -44", 0, 44, "Unknown system error -44"),
    (45, 0, 45, "Unknown system error -45", 0, 45, "Unknown system error -45"),
    (46, 0, 46, "Unknown system error -46", 0, 46, "Unknown system error -46"),
    (47, 0, 47, "Unknown system error -47", 0, 47, "Unknown system error -47"),
    (48, 0, 48, "Unknown system error -48", 0, 48, "Unknown system error -48"),
    (49, 0, 49, "protocol driver not attached", 0, 49, "protocol driver not attached"),
    (50, 0, 50, "Unknown system error -50", 0, 50, "Unknown system error -50"),
    (51, 0, 51, "Unknown system error -51", 0, 51, "Unknown system error -51"),
    (52, 0, 52, "Unknown system error -52", 0, 52, "Unknown system error -52"),
    (53, 0, 53, "Unknown system error -53", 0, 53, "Unknown system error -53"),
    (54, 0, 54, "Unknown system error -54", 0, 54, "Unknown system error -54"),
    (55, 0, 55, "Unknown system error -55", 0, 55, "Unknown system error -55"),
    (56, 0, 56, "Unknown system error -56", 0, 56, "Unknown system error -56"),
    (57, 0, 57, "Unknown system error -57", 0, 57, "Unknown system error -57"),
    (58, 0, 58, "Unknown system error -58", 0, 58, "Unknown system error -58"),
    (59, 0, 59, "Unknown system error -59", 0, 59, "Unknown system error -59"),
    (60, 3, 60, "invalid argument", 0, 60, "Unknown system error -60"),
    (61, 12, 61, "no data available", 12, 61, "no data available"),
    (62, 20, 62, "connection timed out", 0, 62, "Unknown system error -62"),
    (63, 8, 63, "no buffer space available", 0, 63, "Unknown system error -63"),
    (64, 0, 64, "machine is not on the network", 0, 64, "machine is not on the network"),
    (65, 0, 65, "Unknown system error -65", 0, 65, "Unknown system error -65"),
    (66, 0, 66, "Unknown system error -66", 0, 66, "Unknown system error -66"),
    (67, 18, 67, "connection reset by peer", 0, 67, "Unknown system error -67"),
    (68, 0, 68, "Unknown system error -68", 0, 68, "Unknown system error -68"),
    (69, 0, 69, "Unknown system error -69", 0, 69, "Unknown system error -69"),
    (70, 0, 70, "Unknown system error -70", 0, 70, "Unknown system error -70"),
    (71, 19, 71, "protocol error", 19, 71, "protocol error"),
    (72, 0, 72, "Unknown system error -72", 0, 72, "Unknown system error -72"),
    (73, 0, 73, "Unknown system error -73", 0, 73, "Unknown system error -73"),
    (74, 19, 74, "protocol error", 0, 74, "Unknown system error -74"),
    (75, 0, 75, "value too large for defined data type", 0, 75, "value too large for defined data type"),
    (76, 0, 76, "Unknown system error -76", 0, 76, "Unknown system error -76"),
    (77, 0, 77, "Unknown system error -77", 0, 77, "Unknown system error -77"),
    (78, 0, 78, "Unknown system error -78", 0, 78, "Unknown system error -78"),
    (79, 0, 79, "Unknown system error -79", 0, 79, "Unknown system error -79"),
    (80, 0, 80, "Unknown system error -80", 0, 80, "Unknown system error -80"),
    (81, 0, 81, "Unknown system error -81", 0, 81, "Unknown system error -81"),
    (82, 0, 82, "Unknown system error -82", 0, 82, "Unknown system error -82"),
    (83, 0, 83, "Unknown system error -83", 0, 83, "Unknown system error -83"),
    (84, 3, 84, "illegal byte sequence", 3, 84, "illegal byte sequence"),
    (85, 0, 85, "Unknown system error -85", 0, 85, "Unknown system error -85"),
    (86, 0, 86, "Unknown system error -86", 0, 86, "Unknown system error -86"),
    (87, 0, 87, "Unknown system error -87", 0, 87, "Unknown system error -87"),
    (88, 3, 88, "socket operation on non-socket", 3, 88, "socket operation on non-socket"),
    (89, 3, 89, "destination address required", 3, 89, "destination address required"),
    (90, 8, 90, "message too long", 8, 90, "message too long"),
    (91, 19, 91, "protocol wrong type for socket", 19, 91, "protocol wrong type for socket"),
    (92, 22, 92, "protocol not available", 22, 92, "protocol not available"),
    (93, 19, 93, "protocol not supported", 19, 93, "protocol not supported"),
    (94, 0, 94, "socket type not supported", 0, 94, "socket type not supported"),
    (95, 22, 95, "operation not supported on socket", 22, 95, "operation not supported on socket"),
    (96, 0, 96, "Unknown system error -96", 0, 96, "Unknown system error -96"),
    (97, 22, 97, "address family not supported", 22, 97, "address family not supported"),
    (98, 21, 98, "address already in use", 21, 98, "address already in use"),
    (99, 22, 99, "address not available", 22, 99, "address not available"),
    (100, 18, 100, "network is down", 18, 100, "network is down"),
    (101, 12, 101, "network is unreachable", 12, 101, "network is unreachable"),
    (102, 18, 102, "connection reset by peer", 0, 102, "Unknown system error -102"),
    (103, 6, 103, "software caused connection abort", 6, 103, "software caused connection abort"),
    (104, 18, 104, "connection reset by peer", 18, 104, "connection reset by peer"),
    (105, 8, 105, "no buffer space available", 8, 105, "no buffer space available"),
    (106, 14, 106, "socket is already connected", 14, 106, "socket is already connected"),
    (107, 3, 107, "socket is not connected", 3, 107, "socket is not connected"),
    (108, 0, 108, "cannot send after transport endpoint shutdown", 0, 108, "cannot send after transport endpoint shutdown"),
    (109, 0, 109, "Unknown system error -109", 0, 109, "Unknown system error -109"),
    (110, 20, 110, "connection timed out", 20, 110, "connection timed out"),
    (111, 12, 111, "connection refused", 12, 111, "connection refused"),
    (112, 0, 112, "host is down", 0, 112, "host is down"),
    (113, 12, 113, "host is unreachable", 12, 113, "host is unreachable"),
    (114, 0, 114, "connection already in progress", 0, 114, "connection already in progress"),
    (115, 14, 115, "socket is already connected", 0, 115, "Unknown system error -115"),
    (116, 0, 116, "Unknown system error -116", 0, 116, "Unknown system error -116"),
    (117, 0, 117, "Unknown system error -117", 0, 117, "Unknown system error -117"),
    (118, 0, 118, "Unknown system error -118", 0, 118, "Unknown system error -118"),
    (119, 0, 119, "Unknown system error -119", 0, 119, "Unknown system error -119"),
    (120, 0, 120, "Unknown system error -120", 0, 120, "Unknown system error -120"),
    (121, 0, 121, "remote I/O error", 0, 121, "remote I/O error"),
    (122, 0, 122, "Unknown system error -122", 0, 122, "Unknown system error -122"),
    (123, 0, 123, "Unknown system error -123", 0, 123, "Unknown system error -123"),
    (124, 0, 124, "Unknown system error -124", 0, 124, "Unknown system error -124"),
    (125, 0, 125, "operation canceled", 0, 125, "operation canceled"),
    (126, 0, 126, "Unknown system error -126", 0, 126, "Unknown system error -126"),
    (127, 0, 127, "Unknown system error -127", 0, 127, "Unknown system error -127"),
    (128, 0, 128, "Unknown system error -128", 0, 128, "Unknown system error -128"),
    (129, 0, 129, "Unknown system error -129", 0, 129, "Unknown system error -129"),
    (130, 0, 130, "Unknown system error -130", 0, 130, "Unknown system error -130"),
    (131, 0, 131, "Unknown system error -131", 0, 131, "Unknown system error -131"),
    (132, 0, 132, "Unknown system error -132", 0, 132, "Unknown system error -132"),
    (133, 0, 133, "Unknown system error -133", 0, 133, "Unknown system error -133"),
    (134, 0, 134, "Unknown system error -134", 0, 134, "Unknown system error -134"),
    (135, 0, 135, "Unknown system error -135", 0, 135, "Unknown system error -135"),
    (136, 0, 136, "Unknown system error -136", 0, 136, "Unknown system error -136"),
    (137, 0, 137, "Unknown system error -137", 0, 137, "Unknown system error -137"),
    (138, 0, 138, "Unknown system error -138", 0, 138, "Unknown system error -138"),
    (139, 0, 139, "Unknown system error -139", 0, 139, "Unknown system error -139"),
    (140, 0, 140, "Unknown system error -140", 0, 140, "Unknown system error -140"),
];

/// Every mismatch is reported at once.
#[test]
fn decode_matches_native() {
    let mut bad = Vec::new();
    for &(e, k, c, d, uk, uc, ud) in NATIVE {
        let io = IoError::decode_io_error(e, Some(b"f"));
        let (c1, d1) = code_details(&io);
        if (kind(&io), c1, d1) != (k, c, d) {
            bad.push(format!(
                "decode_io_error({e}): {io:?}, native ({k}, {c}, {d:?})"
            ));
        }
        let uv = IoError::decode_uv_error(-e, Some(b"f"));
        let (c1, d1) = code_details(&uv);
        if (kind(&uv), c1, d1) != (uk, uc, ud) {
            bad.push(format!(
                "decode_uv_error(-{e}): {uv:?}, native ({uk}, {uc}, {ud:?})"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// LB-03: the classes io.cpp asserts a file name for get `""` without one,
/// instead of native's crash; the others keep or drop the name as native does.
#[test]
fn nameless_errors_do_not_crash() {
    assert_eq!(
        IoError::decode_io_error(ENOENT, None),
        IoError::NoFileOrDirectory(String::new(), 2, "no such file or directory".into())
    );
    assert_eq!(
        IoError::decode_io_error(EINTR, None),
        IoError::Interrupted(String::new(), 4, "interrupted system call".into())
    );
    assert_eq!(
        IoError::decode_uv_error(-ENOENT, None),
        IoError::NoFileOrDirectory(String::new(), 2, "no such file or directory".into())
    );
    assert_eq!(
        IoError::decode_io_error(EBADF, None),
        IoError::InvalidArgument(None, 9, "bad file descriptor".into())
    );
    // io.cpp's release build ignores a file name where it asserts none
    assert_eq!(
        IoError::decode_io_error(EIO, Some(b"f")),
        IoError::HardwareFault(5, "i/o error".into())
    );
}

#[test]
fn messages_and_mapping() {
    assert_eq!(crt_to_uv(EBADMSG), -EPROTO);
    assert_eq!(crt_to_uv(ENOMSG), -ENODATA);
    assert_eq!(crt_to_uv(ENOEXEC), -ENOEXEC);
    assert_eq!(crt_to_uv(4096), -4096);
    assert_eq!(uv_strerror(-4095), "end of file");
    assert_eq!(uv_strerror(-3008), "unknown node or service");
    assert_eq!(uv_strerror(-ENOEXEC), "Unknown system error -8");
    assert_eq!(uv_strerror(0), "Unknown system error 0");
    assert_eq!(
        IoError::embedded_nul(b"a\0b"),
        IoError::InvalidArgument(Some("a\0b".into()), 22, "string contains NUL bytes".into())
    );
    assert_eq!(
        IoError::file_not_found(b"/x"),
        IoError::NoFileOrDirectory("/x".into(), 2, String::new())
    );
    assert_eq!(IoError::<String>::UnexpectedEof.ctor_index(), 17);
    assert_eq!(IoError::user_error("m").ctor_index(), 18);
}

#[test]
fn errno_is_per_thread() {
    set_errno(EBADF);
    assert_eq!(errno(), EBADF);
    std::thread::spawn(|| assert_eq!(errno(), 0))
        .join()
        .unwrap();
    assert_eq!(errno(), EBADF);
}

/// One error of each constructor shape, with and without a file name.
fn samples() -> Vec<IoError> {
    let (f, d) = (|| "f".to_owned(), || "d".to_owned());
    vec![
        IoError::AlreadyExists(None, 17, d()),
        IoError::AlreadyExists(Some(f()), 17, d()),
        IoError::OtherError(1, d()),
        IoError::ResourceBusy(16, d()),
        IoError::ResourceVanished(32, d()),
        IoError::UnsupportedOperation(38, d()),
        IoError::HardwareFault(5, d()),
        IoError::UnsatisfiedConstraints(39, d()),
        IoError::IllegalOperation(25, d()),
        IoError::ProtocolError(71, d()),
        IoError::TimeExpired(110, d()),
        IoError::Interrupted(f(), 4, d()),
        IoError::NoFileOrDirectory(f(), 2, d()),
        IoError::InvalidArgument(None, 22, d()),
        IoError::InvalidArgument(Some(f()), 22, d()),
        IoError::PermissionDenied(None, 13, d()),
        IoError::PermissionDenied(Some(f()), 13, d()),
        IoError::ResourceExhausted(None, 12, d()),
        IoError::ResourceExhausted(Some(f()), 12, d()),
        IoError::InappropriateType(None, 21, d()),
        IoError::InappropriateType(Some(f()), 21, d()),
        IoError::NoSuchThing(None, 6, d()),
        IoError::NoSuchThing(Some(f()), 6, d()),
        IoError::UnexpectedEof,
        IoError::UserError("m".to_owned()),
    ]
}

/// Each error's builder, by name: the one io.cpp's `decode_uv_error_impl`
/// calls for its class (a `_file` builder when it has a file name), and
/// lean2rr's numbering (`IO_ERROR_BUILDERS`, its `ioErrorBuilderSyms`).
#[test]
fn builder_names() {
    let want = [
        Some("lean_mk_io_error_already_exists"),
        Some("lean_mk_io_error_already_exists_file"),
        Some("lean_mk_io_error_other_error"),
        Some("lean_mk_io_error_resource_busy"),
        Some("lean_mk_io_error_resource_vanished"),
        Some("lean_mk_io_error_unsupported_operation"),
        Some("lean_mk_io_error_hardware_fault"),
        Some("lean_mk_io_error_unsatisfied_constraints"),
        Some("lean_mk_io_error_illegal_operation"),
        Some("lean_mk_io_error_protocol_error"),
        Some("lean_mk_io_error_time_expired"),
        Some("lean_mk_io_error_interrupted"),
        Some("lean_mk_io_error_no_file_or_directory"),
        Some("lean_mk_io_error_invalid_argument"),
        Some("lean_mk_io_error_invalid_argument_file"),
        Some("lean_mk_io_error_permission_denied"),
        Some("lean_mk_io_error_permission_denied_file"),
        Some("lean_mk_io_error_resource_exhausted"),
        Some("lean_mk_io_error_resource_exhausted_file"),
        Some("lean_mk_io_error_inappropriate_type"),
        Some("lean_mk_io_error_inappropriate_type_file"),
        Some("lean_mk_io_error_no_such_thing"),
        Some("lean_mk_io_error_no_such_thing_file"),
        None,
        Some("lean_mk_io_user_error"),
    ];
    let samples = samples();
    assert_eq!(samples.len(), want.len());
    for (e, w) in samples.iter().zip(want) {
        let got = e.builder_index().map(|i| IO_ERROR_BUILDERS[usize::from(i)]);
        assert_eq!(got, w, "{e:?}");
    }
    // the numbering lean2rr's shim decodes (`ioErrorOf`): user error 23
    assert_eq!(IoError::user_error("m").builder_index(), Some(23));
    assert_eq!(
        IoError::decode_io_error(EACCES, Some(b"f")).builder_index(),
        Some(6)
    );
    assert_eq!(
        IoError::decode_io_error(EACCES, None).builder_index(),
        Some(5)
    );
}

/// The accessors read Lean's fields: `osCode`, `filename`, `details` (or a
/// user error's message).
#[test]
fn accessors() {
    for e in samples() {
        let (code, name, details) = match &e {
            IoError::UnexpectedEof => (None, None, None),
            IoError::UserError(m) => (None, None, Some(m.as_str())),
            IoError::Interrupted(..) | IoError::NoFileOrDirectory(..) => {
                (e.os_code(), Some("f"), Some("d"))
            }
            _ => {
                let has_name = e
                    .builder_index()
                    .is_some_and(|i| IO_ERROR_BUILDERS[usize::from(i)].ends_with("_file"));
                (e.os_code(), has_name.then_some("f"), Some("d"))
            }
        };
        assert_eq!(e.file_name().map(String::as_str), name, "{e:?}");
        assert_eq!(e.details().map(String::as_str), details, "{e:?}");
        assert_eq!(e.os_code(), code, "{e:?}");
        assert_eq!(
            e.os_code().is_some(),
            !matches!(e, IoError::UnexpectedEof | IoError::UserError(_))
        );
    }
    let e = IoError::decode_io_error(ENOENT, Some(b"/x"));
    assert_eq!(e.os_code(), Some(2));
    assert_eq!(e.file_name().map(String::as_str), Some("/x"));
    assert_eq!(
        e.details().map(String::as_str),
        Some("no such file or directory")
    );
}

/// A glue's own string type: `map_str` maps every string in field order
/// and keeps the constructor, the code and the builder; `?` converts
/// through `IoText`.
#[test]
fn own_string_type() {
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Text(String);
    impl IoText for Text {
        fn from_io_text(s: String) -> Text {
            Text(s)
        }
    }
    for e in samples() {
        let mut seen = Vec::new();
        let m: IoError<Text> = e.clone().map_str(|s| {
            seen.push(s.clone());
            Text(s)
        });
        let want: Vec<String> = e
            .file_name()
            .into_iter()
            .chain(e.details())
            .cloned()
            .collect();
        assert_eq!(seen, want, "{e:?}");
        assert_eq!(m.ctor_index(), e.ctor_index());
        assert_eq!(m.builder_index(), e.builder_index());
        assert_eq!(m.os_code(), e.os_code());
        assert_eq!(m.clone().map_str(|t| t.0), e);
        assert_eq!(IoError::<Text>::from(e.clone()), m);
    }
    fn fails() -> Result<(), IoError<Text>> {
        Err(IoError::decode_io_error(ENOENT, None))?;
        Ok(())
    }
    assert_eq!(
        fails(),
        Err(IoError::NoFileOrDirectory(
            Text(String::new()),
            2,
            Text("no such file or directory".into())
        ))
    );
}
