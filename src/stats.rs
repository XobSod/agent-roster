//! What agents can tell about a session beyond its state: prompts, context use,
//! tokens, cost, changed lines. Every field is optional; each agent fills in what
//! it can. Claude's come from its transcript, an internal format that may change,
//! so whatever can't be read is simply left out.

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How many recent prompts are kept, for generating a title.
const RECENT_PROMPTS: usize = 5;

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

/// Reads a Claude transcript as it grows, keeping running totals.
#[derive(Debug, Default)]
pub struct ClaudeTranscript {
    path: PathBuf,
    offset: u64,
    /// An incomplete last line, finished by the next read.
    rest: Vec<u8>,
    /// Output tokens per reply; Claude writes a reply several times.
    outputs: HashMap<String, u64>,
    context: Option<u64>,
    prompts: u64,
    recent: VecDeque<String>,
    /// The last cost-state: cost, lines added, lines removed.
    cost: Option<(f64, u64, u64)>,
    replies_since_cost: bool,
}

impl ClaudeTranscript {
    pub fn new(path: &Path) -> ClaudeTranscript {
        ClaudeTranscript {
            path: path.to_path_buf(),
            ..ClaudeTranscript::default()
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads what was added since the last call (all of it the first time).
    pub fn update(&mut self) -> std::io::Result<()> {
        let mut file = File::open(&self.path)?;
        let len = file.metadata()?.len();
        if len < self.offset {
            // Rewritten: start over.
            *self = ClaudeTranscript::new(&self.path.clone());
        }
        if len == self.offset {
            return Ok(());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut added = Vec::new();
        file.read_to_end(&mut added)?;
        self.offset += added.len() as u64;
        self.rest.extend_from_slice(&added);
        let mut start = 0;
        while let Some(end) = self.rest[start..].iter().position(|b| *b == b'\n') {
            let line = self.rest[start..start + end].to_vec();
            self.line(&line);
            start += end + 1;
        }
        self.rest.drain(..start);
        Ok(())
    }

    fn line(&mut self, line: &[u8]) {
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        match entry["type"].as_str() {
            Some("assistant") => {
                let message = &entry["message"];
                let usage = &message["usage"];
                if let (Some(id), Some(output)) =
                    (message["id"].as_str(), usage["output_tokens"].as_u64())
                {
                    self.outputs.insert(id.to_string(), output);
                }
                let context: u64 = [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .filter_map(|k| usage[*k].as_u64())
                .sum();
                if context > 0 {
                    self.context = Some(context);
                }
                if self.cost.is_some() {
                    self.replies_since_cost = true;
                }
            }
            Some("user") if entry["isMeta"].as_bool() != Some(true) => {
                // A typed prompt is text; tool results are lists, and local
                // commands and their output are wrapped in tags.
                if let Some(text) = entry["message"]["content"].as_str()
                    && !text.trim_start().starts_with('<')
                {
                    self.prompts += 1;
                    if self.recent.len() == RECENT_PROMPTS {
                        self.recent.pop_front();
                    }
                    self.recent.push_back(text.to_string());
                }
            }
            Some("cost-state") => {
                if let Some(cost) = entry["totalCostUSD"].as_f64() {
                    let added = entry["totalLinesAdded"].as_u64().unwrap_or(0);
                    let removed = entry["totalLinesRemoved"].as_u64().unwrap_or(0);
                    self.cost = Some((cost, added, removed));
                    self.replies_since_cost = false;
                }
            }
            _ => {}
        }
    }

    /// The totals so far; `model` (as Claude reports it) gives the context window.
    pub fn stats(&self, model: Option<&str>) -> AgentStats {
        let window = model.map(|m| {
            if m.ends_with("[1m]") {
                1_000_000
            } else {
                200_000
            }
        });
        AgentStats {
            prompts: Some(self.prompts),
            context_used: self.context,
            context_limit: self.context.and(window),
            output_tokens: (!self.outputs.is_empty()).then(|| self.outputs.values().sum()),
            cost_usd: self.cost.map(|c| c.0),
            lines_added: self.cost.map(|c| c.1),
            lines_removed: self.cost.map(|c| c.2),
            partial: self.replies_since_cost,
        }
    }

    /// The last few prompts the user typed, oldest first.
    pub fn recent_prompts(&self) -> Vec<String> {
        self.recent.iter().cloned().collect()
    }
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

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    fn reply(id: &str, output: u64, cached: u64) -> String {
        line(
            serde_json::json!({"type": "assistant", "message": {"id": id, "usage": {
            "input_tokens": 2, "cache_read_input_tokens": cached, "cache_creation_input_tokens": 10, "output_tokens": output}}}),
        )
    }

    fn prompt(text: &str) -> String {
        line(serde_json::json!({"type": "user", "message": {"content": text}}))
    }

    #[test]
    fn totals_count_each_reply_once_and_follow_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut text = prompt("fix the tests");
        // Claude writes a reply once per content block, with the same usage.
        text += &reply("m1", 500, 1000);
        text += &reply("m1", 500, 1000);
        text += &line(
            serde_json::json!({"type": "user", "message": {"content": [{"type": "tool_result"}]}}),
        );
        text += &prompt("<command-name>/clear</command-name>");
        std::fs::write(&path, &text).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(Some("claude-opus-5-5[1m]"));
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_used),
            (Some(1), Some(500), Some(1012))
        );
        assert_eq!(s.context_limit, Some(1_000_000));
        assert_eq!((s.cost_usd, s.partial), (None, false));

        // More lines later, the last one still being written.
        let more = prompt("now add a retry")
            + &reply("m2", 300, 5000)
            + &line(
                serde_json::json!({"type": "cost-state", "totalCostUSD": 3.2, "totalLinesAdded": 420, "totalLinesRemoved": 35}),
            );
        let half = reply("m3", 100, 6000);
        let (a, b) = half.split_at(20);
        std::fs::write(&path, format!("{text}{more}{a}")).unwrap();
        t.update().unwrap();
        let s = t.stats(Some("claude-haiku-5-5"));
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_limit),
            (Some(2), Some(800), Some(200_000))
        );
        assert_eq!(
            (s.cost_usd, s.lines_added, s.lines_removed, s.partial),
            (Some(3.2), Some(420), Some(35), false)
        );
        std::fs::write(&path, format!("{text}{more}{a}{b}")).unwrap();
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.output_tokens, s.context_used, s.partial),
            (Some(900), Some(6012), true)
        );
        assert_eq!(t.recent_prompts(), ["fix the tests", "now add a retry"]);
    }

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
