//! How system values read on the device's screen.

use alloc::{format, string::String};

const DAY_MS: u64 = 86_400_000;
const WEEKDAYS: [&str; 7] = ["周四", "周五", "周六", "周日", "周一", "周二", "周三"];

/// A byte count in the largest unit that keeps it above one.
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// When `at` was, from `now` (both UTC Unix milliseconds): the time today,
/// 昨天, the weekday within a week, else the date.
pub(crate) fn when(at: u64, now: Option<u64>) -> String {
    let day = at / DAY_MS;
    let minutes = at / 60_000;
    let today = now.map(|now| now / DAY_MS);
    match today.map(|today| today.saturating_sub(day)) {
        Some(0) => format!("{:02}:{:02}", (minutes / 60) % 24, minutes % 60),
        Some(1) => String::from("昨天"),
        Some(2..=6) => String::from(WEEKDAYS[(day % 7) as usize]),
        _ => {
            let (_, month, date) = civil(day);
            format!("{month}月{date}日")
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

/// Signal bars, 1 to 4, for a received strength in dBm.
pub(crate) fn bars(dbm: Option<i8>) -> u8 {
    match dbm {
        None => 2,
        Some(dbm) if dbm >= -55 => 4,
        Some(dbm) if dbm >= -67 => 3,
        Some(dbm) if dbm >= -78 => 2,
        Some(_) => 1,
    }
}

/// What starts a Workflow, from its Event rule.
pub(crate) fn trigger(rule: &str) -> String {
    match rule.split('.').next() {
        Some("time" | "scheduler") => String::from("定时"),
        Some("wifi") => String::from("网络变化时"),
        Some("app") => String::from("应用事件"),
        Some("agent") => String::from("Agent 事件"),
        Some("imessage") => String::from("收到消息时"),
        _ => format!("事件 {rule}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_their_unit() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(310 * 1024 * 1024), "310 MB");
        assert_eq!(size(1_900 * 1024 * 1024), "1.9 GB");
    }

    #[test]
    fn times_read_relative_to_today() {
        // 2026-10-15 (a Thursday) 10:42 UTC.
        let now = 1_792_060_920_000;
        assert_eq!(when(now - 11 * 60_000, Some(now)), "10:31");
        assert_eq!(when(now - DAY_MS, Some(now)), "昨天");
        assert_eq!(when(now - 3 * DAY_MS, Some(now)), "周一");
        assert_eq!(when(now - 30 * DAY_MS, Some(now)), "9月15日");
        assert_eq!(civil(0), (1970, 1, 1));
    }

    #[test]
    fn signal_reads_as_bars() {
        assert_eq!(bars(Some(-40)), 4);
        assert_eq!(bars(Some(-70)), 2);
        assert_eq!(bars(Some(-90)), 1);
    }
}
