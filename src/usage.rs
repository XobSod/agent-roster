//! What the account an agent runs on has used of its limits, as the agent
//! tells it (`Adapter::usage`): each window (five hours, a week), how much of
//! it is used, and when it starts over.

use serde::{Deserialize, Serialize};

use crate::proxy::Account;
use crate::util::{local_when, until};

/// What's said where the accounts would be asked, but `account_limits` is off.
pub const OFF: &str = "Roster doesn't ask your accounts: account_limits is off (`roster config set account_limits true`, or in the settings view)";

/// One round of asking: whether through a CLI Proxy, and what each said.
pub struct Asked {
    pub via: Via,
    pub answers: Vec<Answer>,
}

/// What one agent, or one account a CLI Proxy holds, said.
pub struct Answer {
    /// The agent asked; for an account of the CLI Proxy's, its provider.
    pub agent: String,
    /// The CLI Proxy's account it's about; None for an agent's own.
    pub account: Option<Account>,
    /// What it has used; None for an account roster doesn't ask (disabled,
    /// or of a service it can't ask) or that has no plan's limits.
    pub said: anyhow::Result<Option<Usage>>,
}

/// Whether a round went through a CLI Proxy.
#[derive(Clone, Debug, PartialEq)]
pub enum Via {
    /// None is to be asked: the agents are.
    Agents,
    /// The one at this URL, for the services it holds accounts of; the
    /// agents for the others.
    Proxy(String),
    /// The one at `url` couldn't be asked (`why`): the agents were instead.
    FellBack { url: String, why: String },
    /// The one at `url` couldn't be asked (`why`), having answered before:
    /// what it said then stands, and the agents of its services aren't asked.
    Failed { url: String, why: String },
}

impl Via {
    /// Why the CLI Proxy couldn't be asked, if it couldn't.
    pub fn why(&self) -> Option<&str> {
        match self {
            Via::FellBack { why, .. } | Via::Failed { why, .. } => Some(why),
            _ => None,
        }
    }

    /// What the round went through, in a line: `via CLI Proxy <url>`, or why
    /// it didn't.
    pub fn said(&self) -> Option<String> {
        Some(match self {
            Via::Agents => return None,
            Via::Proxy(url) => format!("via CLI Proxy {url}"),
            Via::FellBack { url, why } => {
                format!("CLI Proxy {url} not asked, the agents instead: {why}")
            }
            Via::Failed { url, why } => {
                format!("CLI Proxy {url} can't be asked now, as it last said: {why}")
            }
        })
    }
}

/// What a round needs from before: whether the CLI Proxy has answered since
/// it was set (then it isn't fallen back from), the services it held
/// accounts of, and Claude's accounts' plans, asked once (by account id).
#[derive(Clone, Debug, Default)]
pub struct Known {
    pub answered: bool,
    pub held: Vec<String>,
    pub plans: std::collections::HashMap<String, String>,
}

/// The order services show in: Claude's, Codex's, then the others by name.
pub fn rank(provider: &str) -> (u8, &str) {
    match provider {
        "claude" => (0, ""),
        "codex" => (1, ""),
        other => (2, other),
    }
}

/// What an account is called where there's room: `Label (email)`, its
/// email, else its service and the proxy's id for it.
pub fn account_title(account: &Account, label: Option<&str>) -> String {
    match (label, &account.email) {
        (Some(label), Some(email)) => format!("{label} ({email})"),
        (Some(label), None) => label.to_string(),
        (None, Some(email)) => email.clone(),
        (None, None) => format!("{} {}", account.provider, account.id),
    }
}

/// What an account is called where there's little room: its label, else
/// its email up to the `@`, else the proxy's id for it.
pub fn account_short(account: &Account, label: Option<&str>) -> String {
    match (label, &account.email) {
        (Some(label), _) => label.to_string(),
        (None, Some(email)) => email.split('@').next().unwrap_or(email).to_string(),
        (None, None) => account.id.clone(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub minutes: u64,
    /// How much of it is used, in percent: at 100, the agent is stopped
    /// until it starts over.
    pub used: f64,
    /// When it starts over, in ms; None if not said.
    pub resets_at: Option<i64>,
}

/// How a window is used against its time going by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    /// Slower: more of it could be used before it starts over.
    Slow,
    /// About as fast.
    Even,
    /// Faster: at this rate it runs out before it starts over.
    Fast,
    /// Used up.
    Out,
}

/// How far, in points, use may be ahead of or behind the window's time and
/// still be even with it: five hours' 15 minutes, a week's 8 hours.
const EVEN: f64 = 5.0;

impl Pace {
    /// Its mark: the rate it's used at up or down, or used up.
    pub fn mark(self) -> &'static str {
        match self {
            Pace::Slow => "▼",
            Pace::Even => "◆",
            Pace::Fast => "▲",
            Pace::Out => "✕",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Pace::Slow => "slow",
            Pace::Even => "on pace",
            Pace::Fast => "fast",
            Pace::Out => "used up",
        }
    }
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
        let left = self.resets_at?.saturating_sub(now).max(0) as f64;
        let length = self.minutes.max(1).saturating_mul(60_000) as f64;
        Some((1.0 - left / length).clamp(0.0, 1.0))
    }

    /// How it's used against its time; None once it has started over, or
    /// not knowing when it does, unless it's used up: as soon as what's left
    /// shows as 0% (`percent`).
    pub fn pace(&self, now: i64) -> Option<Pace> {
        if self.over(now) {
            return None;
        }
        if self.left() < 1.0 {
            return Some(Pace::Out);
        }
        let ahead = self.used - self.elapsed(now)? * 100.0;
        Some(if ahead > EVEN {
            Pace::Fast
        } else if ahead < -EVEN {
            Pace::Slow
        } else {
            Pace::Even
        })
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
            format!(
                "resets {}, in {}",
                local_when(at, now),
                until(at.saturating_sub(now))
            )
        })
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
    fn a_window_is_used_slower_or_faster_than_its_time_goes() {
        // A five-hour window, an hour (20%) gone.
        let at = |used| window(300, used, 4 * HOUR).pace(0);
        assert_eq!(at(14.9), Some(Pace::Slow));
        // Within five points of its time: even with it.
        assert_eq!(at(15.1), Some(Pace::Even));
        assert_eq!(at(20.0), Some(Pace::Even));
        assert_eq!(at(24.9), Some(Pace::Even));
        assert_eq!(at(25.1), Some(Pace::Fast));
        assert_eq!(at(100.0), Some(Pace::Out));
        // 0% left as shown, rounded down: used up, not merely fast.
        assert_eq!(at(99.6), Some(Pace::Out));
        assert_eq!(window(300, 99.6, 4 * HOUR).percent(), "0%");
        assert_eq!(at(99.0), Some(Pace::Fast));
        let half = window(300, 50.0, 150 * 60_000);
        assert_eq!(half.elapsed(0), Some(0.5));
        // Started over since: what it said no longer holds.
        let over = window(300, 100.0, 0);
        assert!(over.over(0));
        assert_eq!(over.pace(0), None);
        // Not knowing when it starts over, only used up tells.
        let unknown = |used| {
            Window {
                resets_at: None,
                ..window(300, used, 0)
            }
            .pace(0)
        };
        assert_eq!(unknown(90.0), None);
        assert_eq!(unknown(100.0), Some(Pace::Out));
    }
}
