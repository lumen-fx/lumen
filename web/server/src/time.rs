//! Writing a moment down, for a `Date` header and a log line.

use std::time::{SystemTime, UNIX_EPOCH};

/// A moment broken into a UTC calendar date and a time of day.
struct Civil {
    year: i64,
    month: u32,
    day: u32,
    weekday: usize,
    hour: u64,
    minute: u64,
    second: u64,
    millis: u32,
}

fn civil(at: SystemTime) -> Civil {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let days = (secs / 86_400) as i64;
    let of_day = secs % 86_400;
    // Howard Hinnant's days-to-civil, which is exact for every day after the
    // epoch.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    Civil {
        year,
        month,
        day,
        // 1970-01-01 was a Thursday.
        weekday: ((days + 4).rem_euclid(7)) as usize,
        hour: of_day / 3_600,
        minute: of_day % 3_600 / 60,
        second: of_day % 60,
        millis: since.subsec_millis(),
    }
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`, the form an HTTP `Date` header takes.
pub(crate) fn http_date(at: SystemTime) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let c = civil(at);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[c.weekday],
        c.day,
        MONTHS[(c.month - 1) as usize],
        c.year,
        c.hour,
        c.minute,
        c.second
    )
}

/// `1994-11-06T08:49:37.000Z`, the form a log line takes.
pub(crate) fn rfc3339(at: SystemTime) -> String {
    let c = civil(at);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_moment_is_written_the_way_http_and_a_log_read_it() {
        // RFC 9110's own example.
        let at = UNIX_EPOCH + Duration::from_secs(784_111_777);
        assert_eq!(http_date(at), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(rfc3339(at), "1994-11-06T08:49:37.000Z");
        // A leap day, and the millisecond.
        let leap = UNIX_EPOCH + Duration::from_millis(951_782_400_123);
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00.123Z");
        assert_eq!(http_date(UNIX_EPOCH), "Thu, 01 Jan 1970 00:00:00 GMT");
    }
}
