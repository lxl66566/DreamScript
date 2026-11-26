//! The platform tz seam: the one world op the datetime face rides (the
//! 2026-11-24 todo 101 round) — the host's local UTC offset for a
//! unix-seconds timestamp, seconds east of UTC (west negative). The
//! calendar math and the fixed-shape formatting compose in @std ds
//! source over this op and the pure walk; nothing here renders fields
//! or strings, the op answers the offset word alone.
//!
//! # The CRT contract
//!
//! The answer comes from the platform C library's local-time row for
//! the timestamp: glibc's `localtime_r` field `tm_gmtoff` (exact over
//! the tz database the host carries, historical rules included), the
//! Windows UCRT pair `localtime_s`/`gmtime_s` with the field
//! difference (the registry rules the CRT reads, exact for the
//! machine's own zone but blind to historical rule changes the
//! database-backed unix answer carries). Both spellings read the
//! host's standing zone choice at the process's first use and the CRT
//! caches it — a host mutating its zone concurrently with calls owns
//! the same race every C program owns, never a new one.
//!
//! `struct tm`'s `tm_year` is a 32-bit int, so a timestamp beyond the
//! ±2.1e9-year window has no CRT answer: the op clamps into the
//! window rather than refusing, because the offset a calendar that
//! far out carries is the host's current zone spelling either way; a
//! CRT that still answers nothing falls back to zero (UTC), the same
//! word an unset zone answers.

use crate::{
    native::{NativeResult, OutputSink, i64_arg},
    object::Heaps,
    value::Value,
    vm::Trap,
};

/// The CRT's honest window, in unix seconds: `struct tm`'s year field
/// is a 32-bit int, so these are the ±(i32 year span) bounds.
const YEAR_FLOOR: i64 = -67_768_036_191_676_800;
const YEAR_CEIL: i64 = 67_768_036_191_676_799;

/// `utc_offset(unix_seconds: int) -> int`: the body behind the
/// platform ABI's `utc_offset` op — the host's local UTC offset for
/// the timestamp, seconds east of UTC. Total by contract: the clamp
/// and the UTC fallback keep every input answering.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn utc_offset_fn(
    _heaps: &mut Heaps,
    _out: &mut OutputSink,
    args: &[Value],
) -> Result<NativeResult, Trap> {
    let seconds = i64_arg(args, 0)?;
    Ok(NativeResult::Word(Value::I64(local_offset(seconds))))
}

/// The offset word for one timestamp, seconds east.
fn local_offset(unix_seconds: i64) -> i64 {
    let t = unix_seconds.clamp(YEAR_FLOOR, YEAR_CEIL);
    imp::local_offset(t)
}

/// The platform spelling of the lookup.
#[cfg(unix)]
mod imp {
    use std::ffi::{c_char, c_int, c_long};

    /// The `time_t` the binding passes: `long` on every unix the
    /// crate's 64-bit ABI stance covers (the ffi reader's own port
    /// note; a 32-bit `_TIME_BITS` port audits it first).
    type TimeT = c_long;

    /// The `struct tm` row through its tail: `localtime_r` writes the
    /// whole struct (the zone name pointer included), so a row headed
    /// only through `gmtoff` lets the CRT's tail write land past the
    /// caller's struct — the offset reads `gmtoff` alone, but the row
    /// must carry every field the CRT fills.
    #[repr(C)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        mon: c_int,
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        gmtoff: c_long,
        zone: *const c_char,
    }

    // the OS binding, the reactor's own no-dependency shape: symbols
    // off the platform libc, no tz crate, no new dependency
    unsafe extern "C" {
        fn localtime_r(time: *const TimeT, result: *mut Tm) -> *mut Tm;
        #[cfg(all(test, target_os = "linux"))]
        fn tzset();
    }

    /// The glibc spelling: `localtime_r`'s own `tm_gmtoff` word, zero
    /// when the CRT answers no row.
    #[allow(clippy::useless_conversion)] // the c_long word widens on a 32-bit unix
    pub(super) fn local_offset(t: i64) -> i64 {
        let time = TimeT::try_from(t).expect("the clamp keeps the window in range");
        let mut tm = Tm {
            sec: 0,
            min: 0,
            hour: 0,
            mday: 0,
            mon: 0,
            year: 0,
            wday: 0,
            yday: 0,
            isdst: 0,
            gmtoff: 0,
            zone: std::ptr::null(),
        };
        let row = unsafe { localtime_r(&raw const time, &raw mut tm) };
        if row.is_null() {
            return 0;
        }
        i64::from(tm.gmtoff)
    }

    /// Re-reads the zone after a `TZ` change — the test seam alone,
    /// never the op body (the CRT's own caching contract stands).
    #[cfg(all(test, target_os = "linux"))]
    pub(super) fn reread_zone() {
        unsafe { tzset() };
    }
}

/// The platform spelling of the lookup.
#[cfg(windows)]
mod imp {
    use std::ffi::c_int;

    /// The `struct tm` row through its tail: the UCRT pair writes the
    /// whole struct (the trailing private fields included), so the row
    /// carries slack past the standard nine the diff reads — the same
    /// full-row rule the unix spelling pins.
    #[repr(C)]
    struct Tm {
        sec: c_int,
        min: c_int,
        hour: c_int,
        mday: c_int,
        mon: c_int,
        year: c_int,
        wday: c_int,
        yday: c_int,
        isdst: c_int,
        pad: c_int,
        tail: [u64; 2],
    }

    // the OS binding, the reactor's own no-dependency shape: the UCRT
    // pair, both writing the caller's row and answering zero on
    // success (the C11 Annex K argument order)
    unsafe extern "C" {
        fn localtime_s(tm: *mut Tm, time: *const i64) -> c_int;
        fn gmtime_s(tm: *mut Tm, time: *const i64) -> c_int;
    }

    /// Reads the row pair and diffs the fields: the true day distance
    /// is exactly -1, 0, or 1 (offsets stay under a day), so a year
    /// wrap's ±364/365 field difference clamps to its sign — the
    /// classic portable spelling over the standard rows alone.
    pub(super) fn local_offset(t: i64) -> i64 {
        let mut local = blank();
        let mut utc = blank();
        let ok_local = unsafe { localtime_s(&raw mut local, &raw const t) };
        let ok_utc = unsafe { gmtime_s(&raw mut utc, &raw const t) };
        if ok_local != 0 || ok_utc != 0 {
            return 0;
        }
        let mut days = local.yday - utc.yday;
        if days > 1 {
            days = 1;
        } else if days < -1 {
            days = -1;
        }
        i64::from(days) * 86_400
            + i64::from(local.hour - utc.hour) * 3_600
            + i64::from(local.min - utc.min) * 60
            + i64::from(local.sec - utc.sec)
    }

    /// One zeroed row, the fields the diff reads.
    fn blank() -> Tm {
        Tm {
            sec: 0,
            min: 0,
            hour: 0,
            mday: 0,
            mon: 0,
            year: 0,
            wday: 0,
            yday: 0,
            isdst: 0,
            pad: 0,
            tail: [0; 2],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offset stays inside the day: every real zone (the database's
    /// historical spellings included) sits within ±24h of UTC.
    #[test]
    fn the_offset_stays_inside_the_day() {
        for probe in [0, 1, -1, 951_782_400, 1_700_000_000, YEAR_FLOOR] {
            assert!(
                local_offset(probe).abs() <= 86_400,
                "the offset for {probe} stays inside the day"
            );
        }
    }

    /// The clamp answers past the CRT's window instead of refusing: the
    /// far edges answer the same word the window's edge answers.
    #[test]
    fn the_far_edges_clamp_into_the_window() {
        assert_eq!(
            local_offset(i64::MIN),
            local_offset(YEAR_FLOOR),
            "the floor edge clamps"
        );
        assert_eq!(
            local_offset(i64::MAX),
            local_offset(YEAR_CEIL),
            "the ceil edge clamps"
        );
    }

    /// A forced POSIX `TZ` names the offset the CRT answers — the exact
    /// spelling the glibc row carries, `TZ=XXX-5` answering five hours
    /// east (the POSIX sign is west-positive, so the east word is the
    /// negated literal). Linux alone: the string's meaning is the
    /// glibc/musl contract the CI host pins.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_forced_posix_tz_answers_its_offset() {
        let had = std::env::var("TZ").ok();
        for (spell, want) in [("XXX0", 0), ("XXX-5", 18_000), ("XXX+3", -10_800)] {
            // test-local: the runner gives each test its own process,
            // and no other unit test reads TZ
            unsafe {
                std::env::set_var("TZ", spell);
            }
            imp::reread_zone();
            assert_eq!(local_offset(0), want, "the {spell} zone answers");
        }
        unsafe {
            match had {
                Some(value) => std::env::set_var("TZ", value),
                None => std::env::remove_var("TZ"),
            }
        }
        imp::reread_zone();
    }
}
