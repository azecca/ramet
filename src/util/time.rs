//! UTC timestamps, formatted without pulling in a calendar library.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_DAY: u64 = 86_400;

/// A point in time broken down into UTC calendar fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UtcDateTime {
    year: i64,
    month: u32,
    day: u32,
    hour: u64,
    minute: u64,
    second: u64,
}

impl UtcDateTime {
    /// The current time.
    pub fn now() -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        Self::from_unix(seconds)
    }

    /// The time `seconds` after the Unix epoch.
    pub fn from_unix(seconds: u64) -> Self {
        let days = seconds / SECONDS_PER_DAY;
        let time = seconds % SECONDS_PER_DAY;
        let (year, month, day) = civil_from_days(days);
        Self {
            year,
            month,
            day,
            hour: time / 3600,
            minute: time % 3600 / 60,
            second: time % 60,
        }
    }

    /// ISO 8601 with an explicit offset, e.g. `2026-09-11T08:30:00+00:00`.
    pub fn to_iso8601(self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// Compact form usable in a name, e.g. `20260911-083000`.
    pub fn to_compact(self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Converts a day count since 1970-01-01 into a (year, month, day) date of the
/// proleptic Gregorian calendar.
///
/// Howard Hinnant's `civil_from_days` algorithm, restricted to dates after the epoch.
fn civil_from_days(days: u64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01, so that leap days end each 400-year era.
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    // Every value is bounded by the arithmetic above (day <= 31, month <= 12).
    (
        i64::try_from(year).unwrap_or(i64::MAX),
        u32::try_from(month).unwrap_or(12),
        u32::try_from(day).unwrap_or(31),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch() {
        assert_eq!(
            UtcDateTime::from_unix(0).to_iso8601(),
            "1970-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn known_dates() {
        // 2000-02-29 12:34:56 UTC, a leap day of a century leap year.
        assert_eq!(
            UtcDateTime::from_unix(951_827_696).to_iso8601(),
            "2000-02-29T12:34:56+00:00"
        );
        // 2026-09-11 08:30:00 UTC.
        let moment = UtcDateTime::from_unix(1_789_115_400);
        assert_eq!(moment.to_iso8601(), "2026-09-11T08:30:00+00:00");
        assert_eq!(moment.to_compact(), "20260911-083000");
    }

    #[test]
    fn end_of_year() {
        // 2024-12-31 23:59:59 UTC.
        assert_eq!(
            UtcDateTime::from_unix(1_735_689_599).to_iso8601(),
            "2024-12-31T23:59:59+00:00"
        );
    }
}
