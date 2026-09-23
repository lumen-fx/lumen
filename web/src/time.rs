//! A moment broken into the UTC calendar date and time of day it falls on.
//!
//! A sitemap's `<lastmod>` is written from this, and so is every date the
//! server that serves a built site writes, so the two never disagree about
//! which day a moment was.

use std::time::{SystemTime, UNIX_EPOCH};

/// A moment as a UTC calendar date and a time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    /// The year, such as 2026.
    pub year: i64,
    /// The month, 1 to 12.
    pub month: u32,
    /// The day of the month, 1 to 31.
    pub day: u32,
    /// The day of the week, 0 for Sunday to 6 for Saturday.
    pub weekday: u32,
    /// The hour, 0 to 23.
    pub hour: u32,
    /// The minute, 0 to 59.
    pub minute: u32,
    /// The second, 0 to 59.
    pub second: u32,
    /// The millisecond, 0 to 999.
    pub millis: u32,
}

/// `at` as a UTC date and time, or `None` for a moment before the epoch,
/// which nothing here has a use for.
pub fn civil(at: SystemTime) -> Option<Civil> {
    let since = at.duration_since(UNIX_EPOCH).ok()?;
    let seconds = since.as_secs();
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Days since the epoch to a civil date, by era: the 400-year cycle is
    // the shortest span the Gregorian rules repeat over, so one division
    // pulls out everything the leap rules touch.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // March-based month, so the leap day falls at the end of the year.
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Some(Civil {
        year,
        month: month as u32,
        day: day as u32,
        // 1970-01-01 was a Thursday.
        weekday: ((days + 4) % 7) as u32,
        hour: (rest / 3_600) as u32,
        minute: (rest % 3_600 / 60) as u32,
        second: (rest % 60) as u32,
        millis: since.subsec_millis(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_moment_falls_on_its_utc_day() {
        let at = |millis| civil(UNIX_EPOCH + Duration::from_millis(millis)).expect("after 1970");
        let epoch = at(0);
        assert_eq!(
            (epoch.year, epoch.month, epoch.day, epoch.weekday),
            (1970, 1, 1, 4)
        );
        // The end of a leap day, which is where a date conversion goes wrong.
        let leap = at(1_709_251_199_999);
        assert_eq!((leap.year, leap.month, leap.day), (2024, 2, 29));
        assert_eq!(
            (leap.hour, leap.minute, leap.second, leap.millis),
            (23, 59, 59, 999)
        );
        // RFC 9110's own example, a Sunday.
        let sunday = at(784_111_777_000);
        assert_eq!(
            (sunday.year, sunday.month, sunday.day, sunday.weekday),
            (1994, 11, 6, 0)
        );
        assert_eq!(civil(UNIX_EPOCH - Duration::from_secs(1)), None);
    }
}
