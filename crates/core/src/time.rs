//! Timestamps as `state.db` stores them: RFC 3339 in UTC with milliseconds,
//! for example `2026-11-02T14:05:09.123Z` (`schema.sql`, conventions).
//!
//! The fixed width makes a text comparison in SQL a time comparison, which
//! the `card_column` view relies on.

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(jiff::Timestamp);

impl Timestamp {
    /// The current time, truncated to the millisecond it is stored with.
    pub fn now() -> Self {
        Self::from_jiff(jiff::Timestamp::now())
    }

    /// Wraps a jiff timestamp, truncated to milliseconds.
    pub fn from_jiff(ts: jiff::Timestamp) -> Self {
        let millis = ts.as_millisecond();
        Timestamp(jiff::Timestamp::from_millisecond(millis).unwrap_or(ts))
    }

    pub fn from_millis(millis: i64) -> Option<Self> {
        jiff::Timestamp::from_millisecond(millis)
            .ok()
            .map(Timestamp)
    }

    pub fn as_jiff(self) -> jiff::Timestamp {
        self.0
    }

    pub fn as_millis(self) -> i64 {
        self.0.as_millisecond()
    }

    /// This time plus a whole number of minutes, saturating at the end of time.
    pub fn plus_minutes(self, minutes: i64) -> Self {
        let span = jiff::SignedDuration::from_mins(minutes);
        Timestamp(self.0.saturating_add(span).unwrap_or(self.0))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.strftime("%Y-%m-%dT%H:%M:%S%.3fZ"))
    }
}

impl FromStr for Timestamp {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<jiff::Timestamp>()
            .map(Self::from_jiff)
            .map_err(|error| Error::Usage(format!("not an RFC 3339 time: {s:?}: {error}")))
    }
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.to_string()))
    }
}

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;
        text.parse()
            .map_err(|error: Error| FromSqlError::Other(Box::new(error)))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Europe/Lisbon wall-clock time without a timezone database, so the binary
/// computes the same inside a Nix build sandbox. Lisbon follows the EU rule:
/// UTC+0 in winter, UTC+1 from 01:00 UTC on the last Sunday of March until
/// 01:00 UTC on the last Sunday of October.
pub mod lisbon {
    use jiff::civil::{self, Weekday};
    use jiff::tz::{Offset, TimeZone};

    use super::Timestamp;

    fn last_sunday_1am_utc(year: i16, month: i8) -> Option<jiff::Timestamp> {
        let mut day = civil::Date::new(year, month, 31).ok()?;
        while day.weekday() != Weekday::Sunday {
            day = day.yesterday().ok()?;
        }
        let at = day.at(1, 0, 0, 0).to_zoned(TimeZone::UTC).ok()?;
        Some(at.timestamp())
    }

    /// The Lisbon offset in hours (0 or 1) at an instant.
    pub fn offset_hours(ts: jiff::Timestamp) -> i8 {
        let year = ts.to_zoned(TimeZone::UTC).year();
        match (last_sunday_1am_utc(year, 3), last_sunday_1am_utc(year, 10)) {
            (Some(start), Some(end)) if ts >= start && ts < end => 1,
            _ => 0,
        }
    }

    /// A fixed zone at `hours` east of UTC.
    pub fn zone(hours: i8) -> TimeZone {
        TimeZone::fixed(Offset::constant(hours))
    }

    /// The instant as a Lisbon wall-clock time.
    pub fn local(ts: jiff::Timestamp) -> civil::DateTime {
        ts.to_zoned(zone(offset_hours(ts))).datetime()
    }

    /// A Lisbon wall-clock time as an instant, or `None` for the hour that
    /// does not exist at the spring changeover. The hour that repeats in
    /// autumn resolves to the earlier instant.
    pub fn from_local(local: civil::DateTime) -> Option<jiff::Timestamp> {
        [1i8, 0].into_iter().find_map(|hours| {
            let zoned = local.to_zoned(zone(hours)).ok()?;
            (offset_hours(zoned.timestamp()) == hours).then(|| zoned.timestamp())
        })
    }

    /// Parses an `--until` style input: RFC 3339 with an offset, a date (09:00
    /// Lisbon), or a date and time without an offset (Lisbon). The board's
    /// away dialog sends the same forms.
    pub fn parse_when(input: &str) -> Result<Timestamp, crate::error::Error> {
        use crate::error::Error;
        let input = input.trim();
        let usage = || {
            Error::Usage(format!(
                "not a time: {input:?} (use 2026-11-07, 2026-11-07T14:00 or an RFC 3339 time with an offset)"
            ))
        };
        let at = |local: civil::DateTime| {
            from_local(local)
                .map(Timestamp::from_jiff)
                .ok_or_else(|| Error::Usage(format!("{local} does not exist in Europe/Lisbon")))
        };
        if let Ok(ts) = input.parse::<jiff::Timestamp>() {
            return Ok(Timestamp::from_jiff(ts));
        }
        if input.contains(['T', 't', ' ']) {
            if let Ok(local) = input.parse::<civil::DateTime>() {
                return at(local);
            }
        } else if let Ok(date) = input.parse::<civil::Date>() {
            return at(date.at(9, 0, 0, 0));
        }
        Err(usage())
    }

    /// The weekly review slot: Saturday 06:00 Lisbon time.
    pub const REVIEW_DAY: Weekday = Weekday::Saturday;
    pub const REVIEW_HOUR: i8 = 6;

    /// The first weekly review slot strictly after `after`.
    pub fn next_review_slot(after: Timestamp) -> Timestamp {
        let mut day = local(after.as_jiff()).date();
        for _ in 0..8 {
            if day.weekday() == REVIEW_DAY
                && let Some(slot) = from_local(day.at(REVIEW_HOUR, 0, 0, 0))
                && slot > after.as_jiff()
            {
                return Timestamp::from_jiff(slot);
            }
            match day.tomorrow() {
                Ok(next) => day = next,
                Err(_) => break,
            }
        }
        after
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_slots_are_saturday_six_lisbon_across_both_changeovers() {
        let slot = |after: &str| lisbon::next_review_slot(after.parse().unwrap()).to_string();
        // Winter: 06:00 Lisbon is 06:00 UTC.
        assert_eq!(slot("2026-11-03T14:00:00Z"), "2026-11-07T06:00:00.000Z");
        // Exactly at a slot, the next one is a week later.
        assert_eq!(slot("2026-11-07T06:00:00Z"), "2026-11-14T06:00:00.000Z");
        assert_eq!(slot("2026-11-07T05:59:59Z"), "2026-11-07T06:00:00.000Z");
        // Summer: 06:00 Lisbon is 05:00 UTC.
        assert_eq!(slot("2026-07-01T12:00:00Z"), "2026-07-04T05:00:00.000Z");
        // March: the clocks go forward on Sunday 2026-03-29.
        assert_eq!(slot("2026-03-28T06:00:00Z"), "2026-04-04T05:00:00.000Z");
        // October: the clocks go back on Sunday 2026-10-25.
        assert_eq!(slot("2026-10-24T05:00:00Z"), "2026-10-31T06:00:00.000Z");
        assert_eq!(slot("2026-10-24T04:59:00Z"), "2026-10-24T05:00:00.000Z");
    }

    #[test]
    fn formats_fixed_width_utc_millis_and_round_trips() {
        let ts: Timestamp = "2026-11-02T14:05:09.123456+01:00".parse().unwrap();
        assert_eq!(ts.to_string(), "2026-11-02T13:05:09.123Z");
        assert_eq!(ts.to_string().parse::<Timestamp>().unwrap(), ts);
        let whole: Timestamp = "2026-11-02T14:05:09Z".parse().unwrap();
        assert_eq!(whole.to_string(), "2026-11-02T14:05:09.000Z");
    }

    #[test]
    fn matches_sqlite_strftime_format() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sqlite: String = conn
            .query_row(
                "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', '2026-11-02 14:05:09.120')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let ts: Timestamp = "2026-11-02T14:05:09.12Z".parse().unwrap();
        assert_eq!(ts.to_string(), sqlite);
    }
}
