//! When a weekly quota reading means "hold the work" (decided 2026-11-05,
//! Thursday). Running ahead of pace is not a reason to stop. The orchestrator
//! holds only when the window would run out before the work could finish, or
//! when under [`HOLD_UNDER_PERCENT`] is left. Otherwise it starts the work and
//! says so in its note to the owner. The spawn floor in `run.start` is a
//! separate, lower check and is not changed by this rule.

use crate::Timestamp;
use crate::domain::{QuotaWindow, Size};

/// Under this share of the window left, new work is held and the owner asked.
pub const HOLD_UNDER_PERCENT: f64 = 10.0;

/// A plain wall-clock estimate of how long one run of a card takes, in
/// minutes. No per-run cost record exists, so the card size stands in: a
/// small card is a couple of hours of agent time, a large one most of a day.
pub fn work_minutes(size: Size) -> i64 {
    match size {
        Size::S => 2 * 60,
        Size::M => 6 * 60,
        Size::L => 12 * 60,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// Less than [`HOLD_UNDER_PERCENT`] of the window is left.
    LowRemaining,
    /// The projected runout comes before the work could finish.
    RunsOutFirst,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Start the work.
    Go,
    /// Start the work; the note to the owner says the window is ahead of pace.
    GoAheadOfPace,
    Hold(Hold),
}

/// The verdict for starting a card of `size` against one weekly window.
pub fn verdict(window: &QuotaWindow, size: Size, now: Timestamp) -> Verdict {
    if window
        .percent_remaining
        .is_some_and(|p| p < HOLD_UNDER_PERCENT)
    {
        return Verdict::Hold(Hold::LowRemaining);
    }
    let Some(empty_at) = window.projected_exhaustion_at else {
        return Verdict::Go;
    };
    // A projection at or after the reset never happens: the week renews first.
    if !window
        .resets_at
        .is_some_and(|resets_at| empty_at < resets_at)
    {
        return Verdict::Go;
    }
    if empty_at < now.plus_minutes(work_minutes(size)) {
        return Verdict::Hold(Hold::RunsOutFirst);
    }
    Verdict::GoAheadOfPace
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(left: f64, empty_at: Option<&str>, resets_at: &str) -> QuotaWindow {
        QuotaWindow {
            label: "claude:seven_day".into(),
            percent_remaining: Some(left),
            resets_at: Some(resets_at.parse().unwrap()),
            pace: Some("ahead".into()),
            reserve: None,
            burn_multiple: None,
            elapsed_percent: None,
            projected_exhaustion_at: empty_at.map(|at| at.parse().unwrap()),
            title: None,
            kind: Some("weekly".into()),
            fetched_at: "2026-11-05T16:00:00Z".parse().unwrap(),
            fetch_error: None,
        }
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    /// The live trial: 56% left, projected empty Sun 2026-11-08 06:27 Lisbon,
    /// reset Tue 2026-11-10 14:00 Lisbon (both UTC+0 in November), a size S card.
    #[test]
    fn trial_numbers_start_the_small_card_and_flag_the_pace() {
        let w = window(56.0, Some("2026-11-08T06:27:00Z"), "2026-11-10T14:00:00Z");
        assert_eq!(
            verdict(&w, Size::S, at("2026-11-05T16:00:00Z")),
            Verdict::GoAheadOfPace
        );
    }

    #[test]
    fn on_pace_starts_without_a_flag() {
        let w = window(56.0, None, "2026-11-10T14:00:00Z");
        assert_eq!(
            verdict(&w, Size::L, at("2026-11-05T16:00:00Z")),
            Verdict::Go
        );
    }

    #[test]
    fn holds_when_the_window_empties_before_the_work_finishes() {
        let w = window(56.0, Some("2026-11-05T17:30:00Z"), "2026-11-10T14:00:00Z");
        let now = at("2026-11-05T16:00:00Z");
        assert_eq!(verdict(&w, Size::S, now), Verdict::Hold(Hold::RunsOutFirst));
        assert_eq!(verdict(&w, Size::M, now), Verdict::Hold(Hold::RunsOutFirst));
    }

    #[test]
    fn size_sets_how_long_the_window_must_last() {
        let w = window(56.0, Some("2026-11-05T20:00:00Z"), "2026-11-10T14:00:00Z");
        let now = at("2026-11-05T16:00:00Z");
        assert_eq!(verdict(&w, Size::S, now), Verdict::GoAheadOfPace);
        assert_eq!(verdict(&w, Size::M, now), Verdict::Hold(Hold::RunsOutFirst));
    }

    #[test]
    fn a_projection_after_the_reset_is_ignored() {
        let w = window(56.0, Some("2026-11-11T00:00:00Z"), "2026-11-10T14:00:00Z");
        assert_eq!(
            verdict(&w, Size::L, at("2026-11-05T16:00:00Z")),
            Verdict::Go
        );
    }

    #[test]
    fn holds_under_ten_percent_even_on_pace() {
        let w = window(9.9, None, "2026-11-10T14:00:00Z");
        assert_eq!(
            verdict(&w, Size::S, at("2026-11-05T16:00:00Z")),
            Verdict::Hold(Hold::LowRemaining)
        );
        let w = window(10.0, None, "2026-11-10T14:00:00Z");
        assert_eq!(
            verdict(&w, Size::S, at("2026-11-05T16:00:00Z")),
            Verdict::Go
        );
    }
}
