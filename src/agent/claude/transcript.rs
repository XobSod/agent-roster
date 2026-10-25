//! Claude's transcript: the JSONL file where Claude Code keeps a conversation.
//! An internal format that may change, so whatever can't be read is simply
//! left out.

use std::collections::HashMap;

use serde::Deserialize;
use serde::de::{Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::stats::{AgentStats, Jsonl, Lines, Prompts, Tally, leading_type};

/// Reads a Claude transcript as it grows.
pub type ClaudeTranscript = Jsonl<Totals>;

/// A Claude transcript's running totals.
#[derive(Debug, Default)]
pub struct Totals {
    /// Output tokens per reply; Claude writes a reply several times.
    outputs: HashMap<String, u64>,
    context: Option<u64>,
    prompts: Prompts,
    /// The last cost-state: cost, lines added, lines removed.
    cost: Option<(f64, u64, u64)>,
    replies_since_cost: bool,
}

impl Lines for Totals {
    fn line(&mut self, line: &[u8]) {
        if !wanted(line) {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let message = entry.message.unwrap_or_default();
        match entry.kind.as_deref() {
            Some("assistant") => {
                let usage = message.usage.unwrap_or_default();
                if let (Some(id), Some(output)) = (message.id, usage.output_tokens) {
                    self.outputs.insert(id, output);
                }
                let context: u64 = [
                    usage.input_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                ]
                .into_iter()
                .flatten()
                .sum();
                if context > 0 {
                    self.context = Some(context);
                }
                if self.cost.is_some() {
                    self.replies_since_cost = true;
                }
            }
            Some("user") if entry.is_meta != Some(true) => {
                // A typed prompt is text; tool results are lists, and local
                // commands and their output are wrapped in tags.
                if let Some(PlainText(Some(text))) = message.content
                    && !text.trim_start().starts_with('<')
                {
                    self.prompts.push(text);
                }
            }
            Some("cost-state") => {
                if let Some(cost) = entry.total_cost {
                    let added = entry.lines_added.unwrap_or(0);
                    let removed = entry.lines_removed.unwrap_or(0);
                    self.cost = Some((cost, added, removed));
                    self.replies_since_cost = false;
                }
            }
            _ => {}
        }
    }
}

impl Tally for Totals {
    /// `model` (as Claude reports it) gives the context window.
    fn stats(&self, model: Option<&str>) -> AgentStats {
        // A window smaller than what's in use is the wrong one.
        let window = self
            .context
            .and_then(|used| model.and_then(context_window).filter(|w| *w >= used));
        AgentStats {
            prompts: Some(self.prompts.count),
            context_used: self.context,
            context_limit: window,
            output_tokens: (!self.outputs.is_empty()).then(|| self.outputs.values().sum()),
            cost_usd: self.cost.map(|c| c.0),
            lines_added: self.cost.map(|c| c.1),
            lines_removed: self.cost.map(|c| c.2),
            partial: self.replies_since_cost,
        }
    }

    fn prompts(&self) -> &Prompts {
        &self.prompts
    }
}

/// What roster reads of a transcript entry; the rest of it is skipped unread.
#[derive(Deserialize)]
struct Entry<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "isMeta")]
    is_meta: Option<bool>,
    message: Option<Message>,
    #[serde(rename = "totalCostUSD")]
    total_cost: Option<f64>,
    #[serde(rename = "totalLinesAdded")]
    lines_added: Option<u64>,
    #[serde(rename = "totalLinesRemoved")]
    lines_removed: Option<u64>,
}

#[derive(Default, Deserialize)]
struct Message {
    id: Option<String>,
    usage: Option<Usage>,
    content: Option<PlainText>,
}

#[derive(Default, Deserialize)]
struct Usage {
    output_tokens: Option<u64>,
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

/// A message's content if it's a string (a typed prompt). Claude writes the
/// rest as lists (tool results, replies, often large; and its own markers,
/// such as an interruption), so unlike `stats::Text` these are skipped
/// whole, without being kept.
struct PlainText(Option<String>);

impl<'de> Deserialize<'de> for PlainText {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<PlainText, D::Error> {
        struct Content;
        impl<'de> Visitor<'de> for Content {
            type Value = PlainText;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a message's content")
            }
            fn visit_str<E>(self, text: &str) -> Result<PlainText, E> {
                Ok(PlainText(Some(text.to_string())))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<PlainText, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(PlainText(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<PlainText, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(PlainText(None))
            }
            fn visit_unit<E>(self) -> Result<PlainText, E> {
                Ok(PlainText(None))
            }
        }
        d.deserialize_any(Content)
    }
}

/// Whether a transcript line can be one roster reads. Claude writes its own
/// bookkeeping (titles, modes, the requests it sent, …) with `type` first, so
/// those lines are told apart without parsing them; in a long transcript
/// they're most of it (46 of 80 MB in one, seen in real use).
fn wanted(line: &[u8]) -> bool {
    leading_type(line).is_none_or(|kind| matches!(kind, b"assistant" | b"user" | b"cost-state"))
}

/// Context windows of Claude's models, from the model catalog built into Claude
/// Code 2.1.304 (it writes the window nowhere roster could read it).
const CLAUDE_WINDOWS: &[(&str, u64)] = &[
    ("claude-3-5-haiku", 200_000),
    ("claude-3-5-sonnet", 200_000),
    ("claude-3-7-sonnet", 200_000),
    ("claude-haiku-4-5", 200_000),
    ("claude-sonnet-4-0", 200_000),
    ("claude-sonnet-4-5", 200_000),
    ("claude-sonnet-4-6", 200_000),
    ("claude-opus-4-0", 200_000),
    ("claude-opus-4-1", 200_000),
    ("claude-opus-4-5", 200_000),
    ("claude-opus-4-6", 200_000),
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-8", 1_000_000),
    ("claude-opus-5", 1_000_000),
    ("claude-opus-5-5", 1_000_000),
    ("claude-sonnet-5", 1_000_000),
    ("claude-sonnet-5-5", 1_000_000),
    ("claude-haiku-5-5", 1_000_000),
    ("claude-fable-5", 1_000_000),
    ("claude-fable-5-1", 1_000_000),
    ("claude-mythos-5", 1_000_000),
    ("claude-mythos-5-1", 1_000_000),
];

/// The context window of a model as Claude reports it: 1M with the `[1m]`
/// marker, else the catalog's (a dated id like `claude-sonnet-4-5-20250929`
/// counts as its model). None for a model the catalog doesn't know.
pub fn context_window(model: &str) -> Option<u64> {
    if model.ends_with("[1m]") {
        return Some(1_000_000);
    }
    CLAUDE_WINDOWS
        .iter()
        .find(|(id, _)| {
            model.strip_prefix(id).is_some_and(|rest| {
                rest.is_empty()
                    || rest.strip_prefix('-').is_some_and(|date| {
                        date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit())
                    })
            })
        })
        .map(|(_, window)| *window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Transcript;

    fn line(v: serde_json::Value) -> String {
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
        let s = t.stats(Some("claude-sonnet-4-6"));
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
    fn bookkeeping_is_skipped_unparsed() {
        // Claude's bookkeeping has `type` first; the entries roster reads are
        // read wherever their `type` is.
        assert!(!wanted(br#"{"type":"api-request","body":"..."}"#));
        assert!(!wanted(br#"{"type":"user-ish","message":{}}"#));
        assert!(wanted(br#"{"type":"cost-state","totalCostUSD":1}"#));
        assert!(wanted(br#"{"parentUuid":null,"type":"api-request"}"#));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let text = [
            r#"{"type":"api-request","message":{"content":"not a prompt"}}"#,
            r#"{"type":"user","message":{"content":"first"}}"#,
            r#"{"parentUuid":null,"message":{"content":"second"},"type":"user"}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#,
        ];
        std::fs::write(&path, text.join("\n") + "\n").unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        assert_eq!(t.stats(None).prompts, Some(2));
        assert_eq!(t.recent_prompts(), ["first", "second"]);
    }

    #[test]
    fn context_windows_come_from_the_model() {
        // Claude 5's models have 1M without the "[1m]" marker (found in real use:
        // "claude-opus-5-5" was shown as 200k while Claude's /context said 1m).
        assert_eq!(context_window("claude-opus-5-5"), Some(1_000_000));
        assert_eq!(context_window("claude-opus-5"), Some(1_000_000));
        assert_eq!(context_window("claude-opus-4-6"), Some(200_000));
        assert_eq!(context_window("claude-opus-4-6[1m]"), Some(1_000_000));
        assert_eq!(context_window("claude-sonnet-4-5-20250929"), Some(200_000));
        assert_eq!(context_window("claude-opus-5-55"), None);
        assert_eq!(context_window("claude-new-6"), None);
        // Unknown, or smaller than what's in use: no limit rather than a wrong one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, reply("m1", 10, 300_000)).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        assert_eq!(t.stats(Some("claude-new-6")).context_limit, None);
        assert_eq!(t.stats(Some("claude-opus-4-6")).context_limit, None);
        assert_eq!(
            t.stats(Some("claude-opus-5-5")).context_limit,
            Some(1_000_000)
        );
    }
}
