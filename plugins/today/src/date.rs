//! A moment as a date and a time of day. Pure, so it is tested natively.

const DAYS: [&str; 7] = [
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
];

/// How far a clock on the wall is from UTC, in minutes: 60 for `+01:00`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Offset(i64);

impl Offset {
    /// The offset written as `+01:00`, `-08:00`, `+0530`, `Z` or `UTC`.
    ///
    /// # Errors
    ///
    /// Says what an offset looks like, if `text` is not one.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("z") || text.eq_ignore_ascii_case("utc") {
            return Ok(Self(0));
        }
        let wrong = || format!("{text:?} is not an offset from UTC: write it as +01:00 or -08:00");
        let (sign, digits) = match text.split_at_checked(1) {
            Some(("+", digits)) => (1, digits),
            Some(("-", digits)) => (-1, digits),
            _ => return Err(wrong()),
        };
        let digits: String = digits.chars().filter(|c| *c != ':').collect();
        let (Some(hours), Some(minutes), 4) = (digits.get(..2), digits.get(2..), digits.len())
        else {
            return Err(wrong());
        };
        let hours: i64 = hours.parse().map_err(|_| wrong())?;
        let minutes: i64 = minutes.parse().map_err(|_| wrong())?;
        if hours > 14 || minutes > 59 {
            return Err(wrong());
        }
        Ok(Self(sign * (hours * 60 + minutes)))
    }

    /// The offset as it is written: `UTC`, or `UTC+01:00`.
    pub fn name(self) -> String {
        if self.0 == 0 {
            return "UTC".to_owned();
        }
        let sign = if self.0 < 0 { '-' } else { '+' };
        let minutes = self.0.abs();
        format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
    }
}

/// A moment on a clock `offset` from UTC.
#[derive(Debug, PartialEq, Eq)]
pub struct Moment {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    weekday: &'static str,
    offset: Offset,
}

impl Moment {
    /// The moment `seconds` after 1970-01-01T00:00:00Z, as a clock `offset` from UTC shows it.
    pub fn at(seconds: u64, offset: Offset) -> Self {
        let local = i64::try_from(seconds).unwrap_or(i64::MAX / 2) + offset.0 * 60;
        let days = local.div_euclid(86_400);
        let of_day = local.rem_euclid(86_400);
        let (year, month, day) = civil(days);
        let weekday = usize::try_from(days.rem_euclid(7))
            .ok()
            .and_then(|index| DAYS.get(index))
            .copied()
            .unwrap_or("Thursday");
        Self {
            year,
            month,
            day,
            hour: of_day / 3600,
            minute: of_day % 3600 / 60,
            second: of_day % 60,
            weekday,
            offset,
        }
    }

    /// The date, as `Thursday 2026-11-26 (UTC)`.
    pub fn date(&self) -> String {
        format!(
            "{} {:04}-{:02}-{:02} ({})",
            self.weekday,
            self.year,
            self.month,
            self.day,
            self.offset.name()
        )
    }

    /// The date and the time, as `Thursday 2026-11-26 14:03:22 (UTC)`.
    pub fn date_and_time(&self) -> String {
        format!(
            "{} {:04}-{:02}-{:02} {:02}:{:02}:{:02} ({})",
            self.weekday,
            self.year,
            self.month,
            self.day,
            self.hour,
            self.minute,
            self.second,
            self.offset.name()
        )
    }
}

/// The year, month and day that is `days` days after 1970-01-01, in the Gregorian calendar
/// (Howard Hinnant's `civil_from_days`).
const fn civil(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
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
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_is_a_weekday_a_date_and_a_time() {
        let utc = Offset::default();
        assert_eq!(
            Moment::at(0, utc).date_and_time(),
            "Thursday 1970-01-01 00:00:00 (UTC)"
        );
        // 2026-11-26T14:03:22Z.
        let moment = Moment::at(1_795_701_802, utc);
        assert_eq!(moment.date(), "Thursday 2026-11-26 (UTC)");
        assert_eq!(moment.date_and_time(), "Thursday 2026-11-26 14:03:22 (UTC)");
        // A leap day, and the day after a year ends.
        assert_eq!(
            Moment::at(1_709_164_800, utc).date(),
            "Thursday 2024-02-29 (UTC)"
        );
        assert_eq!(
            Moment::at(1_735_689_600, utc).date(),
            "Wednesday 2025-01-01 (UTC)"
        );
    }

    #[test]
    fn an_offset_moves_the_clock_and_the_date_with_it() {
        let late = 1_795_737_600 - 1800; // 2026-11-26T23:30:00Z
        let east = Offset::parse("+01:00").unwrap();
        assert_eq!(
            Moment::at(late, east).date_and_time(),
            "Friday 2026-11-27 00:30:00 (UTC+01:00)"
        );
        let west = Offset::parse("-0800").unwrap();
        assert_eq!(
            Moment::at(late, west).date_and_time(),
            "Thursday 2026-11-26 15:30:00 (UTC-08:00)"
        );
        assert_eq!(Offset::parse("+05:30").unwrap().name(), "UTC+05:30");
        assert_eq!(Offset::parse("Z").unwrap(), Offset::default());
        assert_eq!(Offset::parse(" utc ").unwrap().name(), "UTC");
    }

    #[test]
    fn what_is_not_an_offset_is_refused() {
        for text in [
            "",
            "1",
            "01:00",
            "+1",
            "+25:00",
            "+01:75",
            "+ab:cd",
            "Europe/Berlin",
            "+é",
        ] {
            assert!(Offset::parse(text).is_err(), "{text}");
        }
    }
}
