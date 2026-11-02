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

#[cfg(test)]
mod tests {
    use super::*;

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
