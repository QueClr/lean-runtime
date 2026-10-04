//! `timeit`, `Std.Time.Timestamp.now`'s clock and the two Windows time-zone
//! externs (Lean 4.34.0's `io.cpp`: `lean_io_timeit`,
//! `lean_get_current_time`, `lean_windows_get_next_transition`,
//! `lean_get_windows_local_timezone_id_at`). The monotonic clock
//! (`IO.monoMsNow`, `monoNanosNow`) is [`super::env`]'s; `hrtime` is
//! [`super::uvsys`]'s.
//!
//! Sources: leanrs's `rt/leanrs_rt/src/io/time.rs` (`fmt_g3`, `timeit_line`,
//! the Windows errors; probe `validate/io/timeit_child`, `time_format`) and
//! lean2rr's `runtime/prelude.rr` (`l2r_timeit_text`, which formats with
//! glibc's `%.3g`) and `leanrt/src/io.rs` (`realtime_nanos`).

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::error::{IoError, EINVAL};

/// C++'s default floating-point output at precision 3
/// (`std::setprecision(3)`, which is `printf`'s `%.3g`): three significant
/// digits, correctly rounded, trailing zeros and a trailing point dropped;
/// the exponent form (`1.23e+03`) when the decimal exponent is below -4 or at
/// least 3. Rust's `{:.2e}` rounds the exact binary value as glibc does.
pub fn fmt_g3(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    if !x.is_finite() {
        return if x.is_nan() {
            if x.is_sign_negative() {
                "-nan"
            } else {
                "nan"
            }
        } else if x > 0.0 {
            "inf"
        } else {
            "-inf"
        }
        .to_owned();
    }
    let sci = format!("{x:.2e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    fn trim(s: &str) -> &str {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.')
        } else {
            s
        }
    }
    if !(-4..3).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim(mant), exp.unsigned_abs())
    } else {
        let digits = (2 - exp) as usize;
        trim(&format!("{x:.digits$}")).to_owned()
    }
}

/// `timeit`'s line without its newline (`lean_io_timeit`): `msg` up to its
/// first NUL byte (Lean prints `string_cstr(msg)`), a space and the time:
/// below one second in milliseconds followed by `ms`, from one second on in
/// seconds followed by `s`, each through [`fmt_g3`]. `nanos` is the
/// difference of two `steady_clock` readings; Lean converts it to seconds as
/// a `double` (`nanos / 1e9`) and the milliseconds from those (`* 1000`).
pub fn timeit_line(msg: &[u8], nanos: u64) -> Vec<u8> {
    let msg = match msg.iter().position(|&b| b == 0) {
        Some(n) => &msg[..n],
        None => msg,
    };
    let secs = nanos as f64 / 1e9;
    let time = if secs < 1.0 {
        format!(" {}ms", fmt_g3(secs * 1000.0))
    } else {
        format!(" {}s", fmt_g3(secs))
    };
    let mut line = Vec::with_capacity(msg.len() + time.len());
    line.extend_from_slice(msg);
    line.extend_from_slice(time.as_bytes());
    line
}

/// `timeit msg act` (`lean_io_timeit`): runs `act` between two readings of
/// the monotonic clock, then writes [`timeit_line`] and a newline through the
/// calling thread's current standard-error stream (Lean's `io_eprintln`,
/// [`super::debug::runtime_eprintln`]), and returns `act`'s result, an
/// error included.
pub fn timeit<R>(msg: &[u8], act: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let r = act();
    let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    super::debug::runtime_eprintln(&timeit_line(msg, nanos));
    r
}

/// `Std.Time.Timestamp.now` (`lean_get_current_time`): the system clock
/// (`std::chrono::system_clock`, `CLOCK_REALTIME`) as nanoseconds since the
/// Unix epoch, split into seconds and nanoseconds by C++'s truncating `/` and
/// `%` (both negative before the epoch). The translator builds Lean's
/// `Timestamp` from the two `Int`s.
pub fn current_time() -> (i64, i64) {
    let nanos: i64 = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    };
    (nanos / 1_000_000_000, nanos % 1_000_000_000)
}

/// `Std.Time.Database.Windows.getNextTransition`
/// (`lean_windows_get_next_transition`) off Windows: Lean's error.
pub fn windows_get_next_transition(_id: &[u8], _t: i64, _default_time: bool) -> IoError {
    IoError::InvalidArgument(
        None,
        EINVAL as u32,
        "failed to get timezone, its windows only.".to_owned(),
    )
}

/// `Std.Time.Database.Windows.getLocalTimeZoneIdentifierAt`
/// (`lean_get_windows_local_timezone_id_at`) off Windows: Lean's error.
pub fn windows_local_timezone_id_at(_t: i64) -> IoError {
    IoError::InvalidArgument(
        None,
        EINVAL as u32,
        "timezone retrieval is Windows-only".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `%.3g` as glibc prints it (leanrs's `io_timeit`; lean (timeit_child):
    /// `0.0011ms`, `20.1ms`, `0.000448ms`).
    #[test]
    fn g3() {
        for (x, s) in [
            (0.0011, "0.0011"),
            (20.1234, "20.1"),
            (0.000448, "0.000448"),
            (0.0000448, "4.48e-05"),
            (999.6, "1e+03"),
            (999.4, "999"),
            (1234.5, "1.23e+03"),
            (1.5, "1.5"),
            (100.0, "100"),
            (0.0, "0"),
            (0.0001, "0.0001"),
            // rounded to three digits, the exponent is -4: the fixed form
            (0.00009996, "0.0001"),
            (1e300, "1e+300"),
            (123456789.0, "1.23e+08"),
            (0.125, "0.125"),
            (0.1255, "0.126"),
        ] {
            assert_eq!(fmt_g3(x), s, "{x}");
        }
    }

    #[test]
    fn line() {
        assert_eq!(timeit_line(b"msg", 20_100_000), b"msg 20.1ms");
        assert_eq!(timeit_line(b"m\0x", 2_500_000_000), b"m 2.5s");
        assert_eq!(timeit_line(b"t", 999_999_999), b"t 1e+03ms");
        assert_eq!(timeit_line(b"t", 1_000_000_000), b"t 1s");
        assert_eq!(timeit_line(b"", 1_100), b" 0.0011ms");
    }

    #[test]
    #[cfg_attr(miri, ignore)] // the real-time clock under Miri's isolation
    fn now_splits_as_cpp() {
        let (s, n) = current_time();
        assert!(s > 1_700_000_000 && (0..1_000_000_000).contains(&n));
    }

    #[test]
    fn windows_errors() {
        assert_eq!(
            windows_get_next_transition(b"UTC", 0, true),
            IoError::InvalidArgument(
                None,
                22,
                "failed to get timezone, its windows only.".to_owned()
            )
        );
        assert_eq!(
            windows_local_timezone_id_at(0),
            IoError::InvalidArgument(None, 22, "timezone retrieval is Windows-only".to_owned())
        );
    }
}
