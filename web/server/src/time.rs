//! Writing a moment down, for a `Date` header and a log line.

use std::time::SystemTime;

use lumen_web::time::{Civil, civil};

/// The first moment a date is written for.
const EPOCH: Civil = Civil {
    year: 1970,
    month: 1,
    day: 1,
    weekday: 4,
    hour: 0,
    minute: 0,
    second: 0,
    millis: 0,
};

/// `at`, or the epoch for a moment before it, which a clock this server reads
/// does not give.
fn utc(at: SystemTime) -> Civil {
    civil(at).unwrap_or(EPOCH)
}

/// `Sun, 06 Nov 1994 08:49:37 GMT`, the form an HTTP `Date` header takes.
pub(crate) fn http_date(at: SystemTime) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let c = utc(at);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[c.weekday as usize],
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
    let c = utc(at);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

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
