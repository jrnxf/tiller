//! Europe/Lisbon time without a timezone database, so the CLI starts fast and
//! renders the same inside a Nix build sandbox. Lisbon follows the EU rule:
//! UTC+0 in winter, UTC+1 from 01:00 UTC on the last Sunday of March until
//! 01:00 UTC on the last Sunday of October.
//!
//! JSON carries RFC 3339 UTC; TOON shows Lisbon with the weekday
//! (`Tue 2026-11-03 14:00`). A date input without a time means 09:00 Lisbon
//! that day; an input with an offset is taken as given (`protocol.md` 6.1).

use jiff::civil::{self, Weekday};
use jiff::tz::{Offset, TimeZone};
use jiff::{Timestamp, ToSpan};
use serde_json::Value;
use tiller_core::Error;

fn last_sunday_1am_utc(year: i16, month: i8) -> Option<Timestamp> {
    let mut day = civil::Date::new(year, month, 31).ok()?;
    while day.weekday() != Weekday::Sunday {
        day = day.yesterday().ok()?;
    }
    let at = day.at(1, 0, 0, 0).to_zoned(TimeZone::UTC).ok()?;
    Some(at.timestamp())
}

/// The Lisbon offset in hours (0 or 1) at an instant.
fn offset_hours(ts: Timestamp) -> i8 {
    let year = ts.to_zoned(TimeZone::UTC).year();
    match (last_sunday_1am_utc(year, 3), last_sunday_1am_utc(year, 10)) {
        (Some(start), Some(end)) if ts >= start && ts < end => 1,
        _ => 0,
    }
}

fn zone(hours: i8) -> TimeZone {
    TimeZone::fixed(Offset::constant(hours))
}

/// `Tue 2026-11-03 14:00` for an instant, in Lisbon time.
pub fn display(ts: Timestamp) -> String {
    ts.to_zoned(zone(offset_hours(ts)))
        .strftime("%a %Y-%m-%d %H:%M")
        .to_string()
}

/// A Lisbon wall-clock time as an instant.
fn from_local(local: civil::DateTime) -> Result<Timestamp, Error> {
    // Try the summer offset first; the hour that does not exist or repeats at
    // a changeover resolves to the earlier instant.
    for hours in [1i8, 0] {
        if let Ok(zoned) = local.to_zoned(zone(hours))
            && offset_hours(zoned.timestamp()) == hours
        {
            return Ok(zoned.timestamp());
        }
    }
    Err(Error::Usage(format!(
        "{local} does not exist in Europe/Lisbon"
    )))
}

/// Parses a `--until` style input: RFC 3339 with an offset, a date (09:00
/// Lisbon), or a date and time without an offset (Lisbon).
pub fn parse_when(input: &str) -> Result<tiller_core::Timestamp, Error> {
    let input = input.trim();
    let usage = || {
        Error::Usage(format!(
            "not a time: {input:?} (use 2026-11-07, 2026-11-07T14:00 or an RFC 3339 time with an offset)"
        ))
    };
    if let Ok(ts) = input.parse::<Timestamp>() {
        return Ok(tiller_core::Timestamp::from_jiff(ts));
    }
    if input.contains(['T', 't', ' ']) {
        if let Ok(local) = input.parse::<civil::DateTime>() {
            return from_local(local).map(tiller_core::Timestamp::from_jiff);
        }
    } else if let Ok(date) = input.parse::<civil::Date>() {
        return from_local(date.at(9, 0, 0, 0)).map(tiller_core::Timestamp::from_jiff);
    }
    Err(usage())
}

/// Replaces every stored timestamp string in a response with its Lisbon text,
/// for the TOON form.
pub fn localize(value: &mut Value) {
    match value {
        Value::String(text) if is_stored_timestamp(text) => {
            if let Ok(ts) = text.parse::<Timestamp>() {
                *text = display(ts);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(localize),
        Value::Object(map) => map.values_mut().for_each(localize),
        _ => {}
    }
}

/// `2026-11-02T14:05:09.123Z`, the fixed-width form `state.db` stores.
fn is_stored_timestamp(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 24
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b':'
        && bytes[16] == b':'
        && bytes[19] == b'.'
        && bytes[23] == b'Z'
}

/// A span of seconds as a short age: `45s`, `12m`, `3h`, `2d`.
pub fn age(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    match s {
        0..=89 => format!("{s}s"),
        90..=5399 => format!("{}m", (s + 30) / 60),
        5400..=129_599 => format!("{}h", (s + 1800) / 3600),
        _ => format!("{}d", (s + 43_200) / 86_400),
    }
}

/// `ts` plus a whole number of minutes, for thresholds.
pub fn plus_minutes(ts: Timestamp, minutes: i64) -> Timestamp {
    ts.checked_add(minutes.minutes()).unwrap_or(ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn winter_is_utc_and_summer_is_one_hour_ahead() {
        assert_eq!(display(ts("2026-11-03T14:00:00Z")), "Tue 2026-11-03 14:00");
        assert_eq!(display(ts("2026-07-01T14:00:00Z")), "Wed 2026-07-01 15:00");
    }

    #[test]
    fn changeovers_happen_at_one_utc_on_the_last_sunday() {
        // 2026-03-29 and 2026-10-25 are the last Sundays.
        assert_eq!(display(ts("2026-03-29T00:59:59Z")), "Sun 2026-03-29 00:59");
        assert_eq!(display(ts("2026-03-29T01:00:00Z")), "Sun 2026-03-29 02:00");
        assert_eq!(display(ts("2026-10-25T00:59:59Z")), "Sun 2026-10-25 01:59");
        assert_eq!(display(ts("2026-10-25T01:00:00Z")), "Sun 2026-10-25 01:00");
    }

    #[test]
    fn a_bare_date_means_nine_lisbon_that_day() {
        let winter = parse_when("2026-11-07").unwrap();
        assert_eq!(winter.to_string(), "2026-11-07T09:00:00.000Z");
        let summer = parse_when("2026-07-07").unwrap();
        assert_eq!(summer.to_string(), "2026-07-07T08:00:00.000Z");
    }

    #[test]
    fn an_offset_is_taken_as_given_and_a_local_time_is_lisbon() {
        let given = parse_when("2026-11-07T10:00:00+02:00").unwrap();
        assert_eq!(given.to_string(), "2026-11-07T08:00:00.000Z");
        let local = parse_when("2026-07-07T14:30").unwrap();
        assert_eq!(local.to_string(), "2026-07-07T13:30:00.000Z");
        assert!(parse_when("next tuesday").is_err());
    }

    #[test]
    fn stored_timestamps_are_localized_in_place() {
        let mut value = serde_json::json!({"at": "2026-11-03T14:00:00.000Z", "id": "x", "n": [1]});
        localize(&mut value);
        assert_eq!(value["at"], "Tue 2026-11-03 14:00");
        assert_eq!(value["id"], "x");
    }
}
