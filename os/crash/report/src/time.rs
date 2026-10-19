//! Times in reports, in UTC.
//!
//! A report names the moment of the crash in its file name and its header.
//! Both use UTC: the helper and the app agree on it without a time-zone
//! database, and two reports sort by name in the order they happened.

use std::time::{SystemTime, UNIX_EPOCH};

/// The calendar parts of a moment: year, month, day, hour, minute, second.
fn parts(at: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    let secs = at
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    let rem = rem as u32;
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
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

/// `2026-10-18_14-03-22`: a moment as a file name, UTC.
pub fn file_stamp(at: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = parts(at);
    format!("{y:04}-{mo:02}-{d:02}_{h:02}-{mi:02}-{s:02}")
}

/// `2026-10-18 14:03:22 UTC`: a moment as a report reads it.
pub fn display(at: SystemTime) -> String {
    let (y, mo, d, h, mi, s) = parts(at);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_moments_read_back() {
        let at = |s| UNIX_EPOCH + Duration::from_secs(s);
        assert_eq!(display(at(0)), "1970-01-01 00:00:00 UTC");
        assert_eq!(display(at(1_760_796_202)), "2025-10-18 14:03:22 UTC");
        // A leap day, and the last second of a year.
        assert_eq!(file_stamp(at(951_782_400)), "2000-02-29_00-00-00");
        assert_eq!(file_stamp(at(1_798_761_599)), "2026-12-31_23-59-59");
    }
}
