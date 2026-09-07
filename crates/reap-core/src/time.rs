//! Small time and unit helpers.
//!
//! `reap` formats a handful of timestamps and byte counts and needs no calendar
//! arithmetic beyond that, so this is a few dozen lines rather than a
//! dependency on a date-time crate.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, saturating at 0 for pre-epoch times.
pub fn unix_seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

/// Formats a `SystemTime` as an RFC 3339 timestamp in UTC, to whole seconds.
///
/// Used for every timestamp in `--json` output, so the schema stays stable and
/// machine-parseable.
pub fn rfc3339(t: SystemTime) -> String {
    let secs = unix_seconds(t);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Days since 1970-01-01 to a proleptic Gregorian year/month/day.
///
/// Hinnant's `civil_from_days`, which is exact for every date this program can
/// encounter.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Renders a byte count the way the report tables do: three significant figures
/// and a binary unit suffix.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v >= 100.0 {
        format!("{v:.0} {}", UNITS[i])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[i])
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

/// Renders a duration as the coarsest single unit that stays readable.
pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=119 => format!("{s}s"),
        120..=7199 => format!("{}m", s / 60),
        7200..=172_799 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// Parses an RFC 3339 timestamp of the shape [`rfc3339`] produces, plus the
/// common variants a supervisor is likely to write: a numeric offset instead of
/// `Z`, and fractional seconds.
///
/// Returns `None` for anything it does not fully understand, and every caller
/// treats that as "no usable timestamp" rather than guessing.
pub fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't' | b' ') || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u64 = s[11..13].parse().ok()?;
    let minute: u64 = s[14..16].parse().ok()?;
    let second: u64 = s[17..19].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let mut rest = &s[19..];
    // Fractional seconds are accepted and discarded: this program's decisions
    // are never that fine-grained.
    if let Some(stripped) = rest.strip_prefix('.') {
        let digits = stripped.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &stripped[digits..];
    }

    let offset_seconds: i64 = match rest.as_bytes().first() {
        Some(b'Z' | b'z') if rest.len() == 1 => 0,
        Some(sign @ (b'+' | b'-')) if rest.len() == 6 && rest.as_bytes()[3] == b':' => {
            let h: i64 = rest[1..3].parse().ok()?;
            let m: i64 = rest[4..6].parse().ok()?;
            let magnitude = h * 3600 + m * 60;
            if *sign == b'+' {
                magnitude
            } else {
                -magnitude
            }
        }
        // A timestamp with no zone is ambiguous. Refusing it is safer than
        // assuming a zone and getting the age of a heartbeat wrong by hours.
        _ => return None,
    };

    let days = days_from_civil(year, month, day);
    let utc = days * 86_400 + (hour * 3600 + minute * 60 + second) as i64 - offset_seconds;
    if utc < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(utc as u64))
}

/// Inverse of [`civil_from_days`].
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
