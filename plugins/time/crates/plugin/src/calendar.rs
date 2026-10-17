//! UTC calendar dates and Unix time, for the Board's real-time clock.

use barracuda_board_hal::real_time_clock::DateTime;

const MILLIS_PER_DAY: u64 = 86_400_000;

/// Days from 1970-01-01 to the given proleptic Gregorian date.
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The proleptic Gregorian date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u8, day as u8)
}

/// Unix milliseconds of an RTC reading, or `None` when it is not a real date.
pub(crate) fn unix_millis(datetime: DateTime) -> Option<u64> {
    let valid = (1..=12).contains(&datetime.month)
        && (1..=31).contains(&datetime.day)
        && datetime.hour < 24
        && datetime.minute < 60
        && datetime.second < 60;
    if !valid {
        return None;
    }
    let days = days_from_civil(i64::from(datetime.year), datetime.month, datetime.day);
    let seconds = days * 86_400
        + i64::from(datetime.hour) * 3_600
        + i64::from(datetime.minute) * 60
        + i64::from(datetime.second);
    u64::try_from(seconds).ok()?.checked_mul(1_000)
}

/// The UTC calendar date and time at `unix_millis`; weekday 0 is Sunday.
pub(crate) fn datetime(unix_millis: u64) -> Option<DateTime> {
    let days = i64::try_from(unix_millis / MILLIS_PER_DAY).ok()?;
    let seconds_of_day = (unix_millis % MILLIS_PER_DAY) / 1_000;
    let (year, month, day) = civil_from_days(days);
    Some(DateTime {
        year: u16::try_from(year).ok()?,
        month,
        day,
        // 1970-01-01 was a Thursday.
        weekday: ((days + 4) % 7) as u8,
        hour: (seconds_of_day / 3_600) as u8,
        minute: (seconds_of_day / 60 % 60) as u8,
        second: (seconds_of_day % 60) as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_dates_round_trip_through_unix_time() {
        let date = DateTime {
            year: 2026,
            month: 10,
            day: 17,
            weekday: 6,
            hour: 9,
            minute: 41,
            second: 5,
        };
        // 2026-10-17T09:41:05Z, a Saturday.
        assert_eq!(unix_millis(date), Some(1_792_230_065_000));
        assert_eq!(datetime(1_792_230_065_123), Some(date));
        let epoch = datetime(0);
        assert_eq!(epoch.map(|date| (date.year, date.weekday)), Some((1970, 4)));
        // Leap day.
        let leap = datetime(1_709_164_800_000);
        assert_eq!(leap.map(|date| (date.month, date.day)), Some((2, 29)));
        assert_eq!(unix_millis(DateTime { month: 13, ..date }), None);
    }
}
