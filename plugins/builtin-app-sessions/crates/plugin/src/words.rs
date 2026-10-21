//! How conversation times read on the device's screen.

use alloc::{format, string::String};

const DAY_MS: u64 = 86_400_000;
const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// When `at` was, from `now` (both UTC Unix milliseconds): the time today,
/// "Yesterday", the weekday within a week, else the date.
pub(crate) fn when(at: u64, now: Option<u64>) -> String {
    let day = at / DAY_MS;
    let minutes = at / 60_000;
    let today = now.map(|now| now / DAY_MS);
    match today.map(|today| today.saturating_sub(day)) {
        Some(0) => format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60),
        Some(1) => String::from("Yesterday"),
        Some(2..=6) => String::from(WEEKDAYS[(day % 7) as usize]),
        _ => {
            let (_, month, date) = civil(day);
            format!("{} {date}", MONTHS[(month - 1) as usize])
        }
    }
}

/// The year, month, and day of `days` since 1970-01-01.
fn civil(days: u64) -> (i64, u32, u32) {
    // Howard Hinnant's days_from_civil, inverted.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_relative_to_today() {
        // 2026-10-15 (a Thursday) 10:42 UTC.
        let now = 1_792_060_920_000;
        assert_eq!(when(now - 11 * 60_000, Some(now)), "10:31");
        assert_eq!(when(now - DAY_MS, Some(now)), "Yesterday");
        assert_eq!(when(now - 3 * DAY_MS, Some(now)), "Mon");
        assert_eq!(when(now - 30 * DAY_MS, Some(now)), "Sep 15");
        assert_eq!(civil(0), (1970, 1, 1));
    }
}
