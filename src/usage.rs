//! What the account an agent runs on has used of its limits, as the agent's
//! own service tells it (`Adapter::usage`): each window (five hours, a week),
//! how much of it is used, and when it starts over.

use serde::Serialize;

use crate::util::{local_when, until};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Usage {
    /// The account's plan, as the agent names it (pro, plus).
    pub plan: Option<String>,
    /// Shortest first.
    pub windows: Vec<Window>,
    /// What else the agent says of its limits, a line each (codex's reset
    /// credits).
    pub notes: Vec<String>,
    /// When it was so, in ms.
    pub at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Window {
    pub minutes: u64,
    /// How much of it is used, in percent: at 100, the agent is stopped
    /// until it starts over.
    pub used: f64,
    /// When it starts over, in ms; None if not said.
    pub resets_at: Option<i64>,
}

/// How a window is going, from least to most worrying.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pace {
    /// It lasts until it starts over, at the rate it's used.
    Fine,
    /// At least half used, faster than its time goes by: at this rate it runs
    /// out before it starts over. Earlier, one busy hour would say so of a
    /// week.
    Short,
    /// Used up.
    Out,
}

impl Window {
    /// "5 hours", "Weekly", or as long as it is.
    pub fn label(&self) -> String {
        match self.minutes {
            1440 => "Daily".into(),
            10_080 => "Weekly".into(),
            m if m % 1440 == 0 => format!("{} days", m / 1440),
            60 => "1 hour".into(),
            m if m % 60 == 0 => format!("{} hours", m / 60),
            m => format!("{m} minutes"),
        }
    }

    /// Whether it has started over since (`used` no longer holds).
    pub fn over(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|at| at <= now)
    }

    /// How much of it has gone by at `now`, from 0 to 1; None if when it
    /// starts over isn't known.
    pub fn elapsed(&self, now: i64) -> Option<f64> {
        let left = (self.resets_at? - now).max(0) as f64;
        let length = (self.minutes.max(1) * 60_000) as f64;
        Some((1.0 - left / length).clamp(0.0, 1.0))
    }

    pub fn pace(&self, now: i64) -> Pace {
        if self.over(now) {
            return Pace::Fine;
        }
        if self.used >= 100.0 {
            return Pace::Out;
        }
        match self.elapsed(now) {
            Some(gone) if self.used >= 50.0 && self.used > gone * 100.0 => Pace::Short,
            _ => Pace::Fine,
        }
    }

    /// How much of it is left, in percent.
    pub fn left(&self) -> f64 {
        (100.0 - self.used).clamp(0.0, 100.0)
    }

    /// What's left, rounded down so it's never more than there is: "68%".
    pub fn percent(&self) -> String {
        format!("{:.0}%", self.left().floor())
    }

    /// When it starts over: "resets 19:20, in 3h 25m", with its day if not
    /// today; "started over at 17:00" once it has.
    pub fn resets(&self, now: i64) -> Option<String> {
        let at = self.resets_at?;
        Some(if at <= now {
            format!("started over at {}", local_when(at, now))
        } else {
            format!("resets {}, in {}", local_when(at, now), until(at - now))
        })
    }
}

impl Usage {
    /// How its worst window is going.
    pub fn pace(&self, now: i64) -> Pace {
        self.windows
            .iter()
            .map(|w| w.pace(now))
            .max()
            .unwrap_or(Pace::Fine)
    }

    /// What's left of its windows, shortest first: "68% · 48%"; "–" for one
    /// that has started over since.
    pub fn brief(&self, now: i64) -> String {
        self.windows
            .iter()
            .map(|w| {
                if w.over(now) {
                    "–".into()
                } else {
                    w.percent()
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    fn window(minutes: u64, used: f64, resets_in: i64) -> Window {
        Window {
            minutes,
            used,
            resets_at: Some(resets_in),
        }
    }

    #[test]
    fn a_window_is_named_by_its_length() {
        let named = |minutes| window(minutes, 0.0, 0).label();
        assert_eq!(named(300), "5 hours");
        assert_eq!(named(10_080), "Weekly");
        assert_eq!(named(1440), "Daily");
        assert_eq!(named(2880), "2 days");
        assert_eq!(named(60), "1 hour");
        assert_eq!(named(90), "90 minutes");
    }

    #[test]
    fn a_window_runs_short_when_half_used_faster_than_its_time_goes() {
        // A five-hour window, an hour gone.
        let at = |used| window(300, used, 4 * HOUR).pace(0);
        assert_eq!(at(10.0), Pace::Fine);
        // Ahead of its time, but not half used: too early to tell.
        assert_eq!(at(40.0), Pace::Fine);
        assert_eq!(at(55.0), Pace::Short);
        assert_eq!(at(100.0), Pace::Out);
        // Half its time gone, about as much used.
        let half = window(300, 50.0, 150 * 60_000);
        assert_eq!(half.elapsed(0), Some(0.5));
        assert_eq!(half.pace(0), Pace::Fine);
        // Started over since: what it said no longer holds.
        let over = window(300, 100.0, 0);
        assert!(over.over(0));
        assert_eq!(over.pace(0), Pace::Fine);
        // Not knowing when it starts over, only used up counts.
        let unknown = Window {
            resets_at: None,
            ..window(300, 90.0, 0)
        };
        assert_eq!(unknown.pace(0), Pace::Fine);
    }

    #[test]
    fn usage_is_as_bad_as_its_worst_window_and_briefly_whats_left() {
        let usage = Usage {
            plan: None,
            windows: vec![window(300, 31.4, HOUR), window(10_080, 99.9, -HOUR)],
            notes: Vec::new(),
            at: 0,
        };
        assert_eq!(usage.brief(0), "68% · –");
        assert_eq!(usage.pace(0), Pace::Fine);
        let usage = Usage {
            windows: vec![window(300, 31.0, HOUR), window(10_080, 100.0, HOUR)],
            ..usage
        };
        assert_eq!(usage.brief(0), "69% · 0%");
        assert_eq!(usage.pace(0), Pace::Out);
    }
}
