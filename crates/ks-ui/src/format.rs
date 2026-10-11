//! Formatting rules as pure functions: human sizes and relative times.

use std::time::{SystemTime, UNIX_EPOCH};

use ks_core::config::SizeBase;

const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

/// `412 B`, `14.2 KB`, `1.0 MB`. The unit label is `KB` for both bases (`SizeBase::Si`
/// divides by 1000, `SizeBase::Binary` by 1024). Never longer
/// than 8 cells.
pub fn human_size(bytes: u64, base: SizeBase) -> String {
    let divisor = base.divisor() as f64;
    if bytes < base.divisor() {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= divisor && unit < UNITS.len() - 1 {
        value /= divisor;
        unit += 1;
    }
    // 1023.96 KB would print as "1024.0 KB": carry into the next unit instead.
    if (value * 10.0).round() / 10.0 >= divisor && unit < UNITS.len() - 1 {
        value /= divisor;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

fn unix_seconds(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

/// `just now`, `3m ago`, `5h ago`, `yesterday`, then the local date `2025-03-14`.
///
/// "Yesterday" and the date are calendar based, using `utc_offset_secs` to find local
/// midnight. A time slightly in the future (clock skew between machines) counts as "just
/// now"; further ahead it is shown as a date.
pub fn relative_time(time: SystemTime, now: SystemTime, utc_offset_secs: i32) -> String {
    let (then, now) = (unix_seconds(time), unix_seconds(now));
    let age = now - then;
    if age < MINUTE && age > -MINUTE {
        return "just now".to_owned();
    }
    if (MINUTE..HOUR).contains(&age) {
        return format!("{}m ago", age / MINUTE);
    }
    let offset = i64::from(utc_offset_secs);
    let day_of = |t: i64| (t + offset).div_euclid(DAY);
    if (HOUR..DAY).contains(&age) {
        return format!("{}h ago", age / HOUR);
    }
    if age > 0 && day_of(now) - day_of(then) == 1 {
        return "yesterday".to_owned();
    }
    date(day_of(then))
}

/// `2025-03-14 12:00:00` in local time, for the metadata view.
pub fn absolute_time(time: SystemTime, utc_offset_secs: i32) -> String {
    let local = unix_seconds(time) + i64::from(utc_offset_secs);
    let (days, rem) = (local.div_euclid(DAY), local.rem_euclid(DAY));
    format!(
        "{} {:02}:{:02}:{:02}",
        date(days),
        rem / HOUR,
        rem % HOUR / MINUTE,
        rem % MINUTE
    )
}

/// `YYYY-MM-DD` for a count of days since 1970-01-01.
fn date(days_since_epoch: i64) -> String {
    let (year, month, day) = civil_from_days(days_since_epoch);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn sizes_binary() {
        let b = SizeBase::Binary;
        assert_eq!(human_size(0, b), "0 B");
        assert_eq!(human_size(412, b), "412 B");
        assert_eq!(human_size(1023, b), "1023 B");
        assert_eq!(human_size(1024, b), "1.0 KB");
        assert_eq!(human_size(14_540, b), "14.2 KB");
        assert_eq!(human_size(10 * 1024 * 1024, b), "10.0 MB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024, b), "5.0 GB");
        assert_eq!(human_size(1 << 40, b), "1.0 TB");
        assert_eq!(human_size(u64::MAX, b), "16384.0 PB");
    }

    #[test]
    fn sizes_si() {
        let s = SizeBase::Si;
        assert_eq!(human_size(999, s), "999 B");
        assert_eq!(human_size(1000, s), "1.0 KB");
        assert_eq!(human_size(14_200, s), "14.2 KB");
        assert_eq!(human_size(1_500_000, s), "1.5 MB");
    }

    #[test]
    fn rounding_carries_into_the_next_unit() {
        // 1023.96 KiB would otherwise print as "1024.0 KB".
        let almost_a_mebibyte = 1024 * 1024 - 40;
        assert_eq!(human_size(almost_a_mebibyte, SizeBase::Binary), "1.0 MB");
        assert_eq!(human_size(999_960, SizeBase::Si), "1.0 MB");
        assert_eq!(human_size(1_048_000, SizeBase::Binary), "1023.4 KB");
    }

    #[test]
    fn sizes_fit_the_column() {
        for shift in 0..64 {
            let text = human_size(u64::MAX >> shift, SizeBase::Binary);
            assert!(text.len() <= 10, "{text}");
        }
    }

    fn at(secs: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs.unsigned_abs())
    }

    /// 2025-03-14 12:00:00 UTC.
    const NOON: i64 = 1_741_953_600;

    #[test]
    fn recent_times() {
        let now = at(NOON);
        assert_eq!(relative_time(at(NOON - 5), now, 0), "just now");
        assert_eq!(relative_time(at(NOON - 59), now, 0), "just now");
        assert_eq!(relative_time(at(NOON - 60), now, 0), "1m ago");
        assert_eq!(relative_time(at(NOON - 3 * 60), now, 0), "3m ago");
        assert_eq!(relative_time(at(NOON - 59 * 60 - 59), now, 0), "59m ago");
        assert_eq!(relative_time(at(NOON - 3600), now, 0), "1h ago");
        assert_eq!(relative_time(at(NOON - 3 * 3600), now, 0), "3h ago");
        assert_eq!(
            relative_time(at(NOON - 23 * 3600 - 3599), now, 0),
            "23h ago"
        );
    }

    #[test]
    fn yesterday_is_a_calendar_day() {
        let now = at(NOON);
        // 24h ago is still the previous calendar day at noon.
        assert_eq!(relative_time(at(NOON - 24 * 3600), now, 0), "yesterday");
        assert_eq!(relative_time(at(NOON - 35 * 3600), now, 0), "yesterday");
        // The day before yesterday is a date.
        assert_eq!(relative_time(at(NOON - 48 * 3600), now, 0), "2025-03-12");
        // 01:00 local: 23h ago is *yesterday evening*, still reported by hours...
        let early = at(NOON - 11 * 3600);
        assert_eq!(
            relative_time(at(NOON - 11 * 3600 - 23 * 3600), early, 0),
            "23h ago"
        );
        // ...and 25h ago is "yesterday" because it crossed exactly one midnight.
        assert_eq!(
            relative_time(at(NOON - 11 * 3600 - 25 * 3600), early, 0),
            "yesterday"
        );
    }

    #[test]
    fn the_local_offset_moves_midnight() {
        let now = at(NOON);
        let then = at(NOON - 30 * 3600); // 2025-03-13 06:00 UTC
        assert_eq!(relative_time(then, now, 0), "yesterday");
        // In UTC+14 it is already 2025-03-15 02:00 now and the file is from 03-13 20:00:
        // two calendar days apart.
        assert_eq!(relative_time(then, now, 14 * 3600), "2025-03-13");
        // In UTC-12 the file dates from 03-12 18:00 and "now" is 03-14 00:00.
        assert_eq!(relative_time(then, now, -12 * 3600), "2025-03-12");
    }

    #[test]
    fn old_times_are_dates() {
        let now = at(NOON);
        assert_eq!(relative_time(at(0), now, 0), "1970-01-01");
        assert_eq!(relative_time(at(951_782_400), now, 0), "2000-02-29");
        assert_eq!(relative_time(at(NOON - 400 * 86_400), now, 0), "2024-02-08");
    }

    #[test]
    fn times_before_1970_and_in_the_future() {
        let now = at(NOON);
        let before_epoch = UNIX_EPOCH - Duration::from_secs(86_400);
        assert_eq!(relative_time(before_epoch, now, 0), "1969-12-31");
        assert_eq!(relative_time(at(NOON + 30), now, 0), "just now");
        assert_eq!(relative_time(at(NOON + 3 * 86_400), now, 0), "2025-03-17");
    }

    #[test]
    fn calendar_conversion_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(20_149), (2025, 3, 2));
        assert_eq!(date(11_017), "2000-03-01");
    }
}
