//! How settings read on the device's screen.

use alloc::{format, string::String};

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
    fn signal_reads_as_bars() {
        assert_eq!(bars(Some(-40)), 4);
        assert_eq!(bars(Some(-70)), 2);
        assert_eq!(bars(Some(-90)), 1);
        assert_eq!(bars(None), 2);
    }
}
