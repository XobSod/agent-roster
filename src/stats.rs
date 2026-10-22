//! What agents can tell about a session beyond its state: prompts, context use,
//! tokens, cost, changed lines. Every field is optional; each agent fills in what
//! it can, from its transcript (see `Transcript`).

use std::path::Path;

/// How many recent prompts a transcript keeps, for generating a title.
pub const RECENT_PROMPTS: usize = 5;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentStats {
    pub prompts: Option<u64>,
    pub context_used: Option<u64>,
    pub context_limit: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub lines_added: Option<u64>,
    pub lines_removed: Option<u64>,
    /// Cost and lines are totals up to an earlier point; the agent has done more since.
    pub partial: bool,
}

/// An agent's record of a conversation, read as it grows: where roster gets
/// statistics and, for a generated title, the latest prompts.
pub trait Transcript: Send {
    fn path(&self) -> &Path;
    /// Reads what was added since the last call (all of it the first time).
    fn update(&mut self) -> std::io::Result<()>;
    /// The totals so far; `model`, as the agent reports it, may tell the
    /// context window.
    fn stats(&self, model: Option<&str>) -> AgentStats;
    /// The last few prompts the user typed, oldest first.
    fn recent_prompts(&self) -> Vec<String>;
}

/// A session's statistics as short parts, for the header and `roster info`:
/// created 2d · 5 prompts · worked 34m · context 117k/1M (12%) · 92k out ·
/// $3.20 · +420 −35. Parts the agent can't tell are left out; "≥" marks totals
/// that are behind what the agent has done since.
pub fn summary(s: &crate::model::Session, stats: Option<&AgentStats>, now: i64) -> Vec<String> {
    let mut parts = vec![format!("created {}", crate::util::ago(now - s.created_at))];
    match stats.and_then(|a| a.prompts) {
        Some(n) => parts.push(format!("{n} prompt{}", if n == 1 { "" } else { "s" })),
        None if s.turns > 0 => parts.push(format!(
            "{} turn{}",
            s.turns,
            if s.turns == 1 { "" } else { "s" }
        )),
        None => {}
    }
    let worked = s.worked(now);
    if worked > 0 {
        parts.push(format!("worked {}", crate::util::clock(worked)));
    }
    let Some(a) = stats else { return parts };
    match (a.context_used, a.context_limit) {
        (Some(used), Some(limit)) => parts.push(format!(
            "context {}/{} ({}%)",
            tokens(used),
            tokens(limit),
            (used * 100 + limit / 2) / limit.max(1)
        )),
        (Some(used), None) => parts.push(format!("context {}", tokens(used))),
        _ => {}
    }
    if let Some(out) = a.output_tokens {
        parts.push(format!("{} out", tokens(out)));
    }
    let at_least = if a.partial { "≥" } else { "" };
    if let Some(cost) = a.cost_usd {
        parts.push(format!("{at_least}${cost:.2}"));
    }
    if let (Some(added), Some(removed)) = (a.lines_added, a.lines_removed)
        && added + removed > 0
    {
        parts.push(format!("{at_least}+{added} −{removed}"));
    }
    parts
}

/// `1234` → "1.2k", `117000` → "117k", `1000000` → "1M".
pub fn tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_has_what_is_known() {
        let mut s = crate::model::tests::session("a");
        s.created_at = 0;
        s.state = crate::model::State::Idle;
        s.turns = 3;
        s.worked_ms = 34 * 60_000;
        let day = 2 * 86_400_000;
        assert_eq!(
            summary(&s, None, day),
            ["created 2d", "3 turns", "worked 34m"]
        );
        let stats = AgentStats {
            prompts: Some(5),
            context_used: Some(117_000),
            context_limit: Some(1_000_000),
            output_tokens: Some(92_000),
            cost_usd: Some(3.2),
            lines_added: Some(420),
            lines_removed: Some(35),
            partial: true,
        };
        assert_eq!(
            summary(&s, Some(&stats), day).join(" · "),
            "created 2d · 5 prompts · worked 34m · context 117k/1M (12%) · 92k out · ≥$3.20 · ≥+420 −35"
        );
    }

    #[test]
    fn token_counts_are_short() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1_234), "1.2k");
        assert_eq!(tokens(2_000), "2k");
        assert_eq!(tokens(117_400), "117k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(4_660_631), "4.7M");
    }
}
