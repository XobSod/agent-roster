//! What pi spent, call by call, from its session files: each reply's usage,
//! with its provider, model (the one that answered, if the provider said)
//! and what pi says it cost, and the model calls outside the conversation:
//! cache warming (`usage` entries, with their own model), and the summaries
//! of a compaction or of a branch left, of the model last replying; and a
//! subagent's replies, which pi's subagent extension (its example) runs
//! with no session of their own, keeping them in its tool's result.
//! Every branch of the tree counts: it was spent either way. A fork copies
//! the entries it was forked from as they were, their ids and times too,
//! which name the calls, so a copy is one call.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::stats::{Lines, leading_type};
use crate::tokens::{Call, Calls, Counts};
use crate::util::iso_ms;

#[derive(Debug, Default)]
pub struct Replies {
    calls: Vec<Call>,
    /// The last reply's model, as `provider/model`.
    model: Option<String>,
}

impl Lines for Replies {
    fn line(&mut self, line: &[u8]) {
        if !matches!(
            leading_type(line),
            None | Some(b"message" | b"usage" | b"compaction" | b"branch_summary")
        ) || memchr::memmem::find(line, br#""usage""#).is_none()
        {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let at = entry.timestamp.as_deref().and_then(iso_ms);
        let id = format!(
            "pi:{}@{}",
            entry.id.as_deref().unwrap_or(""),
            entry.timestamp.as_deref().unwrap_or("")
        );
        let (usage, at, model) = match entry.kind.as_deref() {
            Some("message") => {
                let Some(message) = entry.message else {
                    return;
                };
                match message.role.as_deref() {
                    Some("assistant") => {}
                    Some("toolResult") => {
                        self.tool_result(&id, at, message);
                        return;
                    }
                    _ => return,
                }
                if let Some(model) =
                    named(message.provider, message.response_model.or(message.model))
                {
                    self.model = Some(model);
                }
                (message.usage, message.timestamp.or(at), None)
            }
            Some("usage") => (entry.usage, at, named(entry.provider, entry.model)),
            Some("compaction" | "branch_summary") => (entry.usage, at, None),
            _ => return,
        };
        if let (Some(usage), Some(at)) = (usage, at) {
            self.push(id, at, model, usage);
        }
    }
}

impl Replies {
    /// A tool's result (`id`'s): what the tool spent itself, as pi has
    /// extensions say it (`usage`, of the model last replying, all of it,
    /// as pi counts it); else a subagent's replies (pi's subagent extension
    /// keeps its runs in `details`), each a call of its own, and its
    /// subagents' in turn. Read leniently: any tool's details are its own.
    fn tool_result(&mut self, id: &str, at: Option<i64>, message: Message) {
        if let Some(usage) = message.usage {
            if let Some(at) = message.timestamp.or(at) {
                self.push(format!("{id}/u"), at, None, usage);
            }
            return;
        }
        let runs = message.details.as_ref().and_then(|d| d.get("results"));
        for (i, run) in runs
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let messages = run.get("messages").and_then(Value::as_array);
            for (j, message) in messages.into_iter().flatten().enumerate() {
                let Ok(message) = Message::deserialize(message) else {
                    continue;
                };
                let id = format!("{id}/{i}/{j}");
                let at = message.timestamp.or(at);
                match message.role.as_deref() {
                    Some("assistant") => {
                        let model =
                            named(message.provider, message.response_model.or(message.model));
                        if let (Some(usage), Some(at)) = (message.usage, at) {
                            self.push(id, at, model, usage);
                        }
                    }
                    Some("toolResult") => self.tool_result(&id, at, message),
                    _ => {}
                }
            }
        }
    }

    /// A call: of `model`, else of the model last replying.
    fn push(&mut self, id: String, at: i64, model: Option<String>, usage: Usage) {
        self.calls.push(Call {
            id,
            at,
            model: model
                .or_else(|| self.model.clone())
                .unwrap_or_else(|| "unknown".into()),
            counts: Counts {
                input: usage.input,
                cache_read: usage.cache_read,
                cache_write: usage.cache_write,
                cache_write_hour: 0,
                output: usage.output,
            },
            cost: usage.cost.and_then(|c| c.total),
            ..Call::default()
        });
    }
}

/// A model as roster names pi's: `provider/model`.
fn named(provider: Option<String>, model: Option<String>) -> Option<String> {
    let model = model?;
    Some(match provider {
        Some(provider) => format!("{provider}/{model}"),
        None => model,
    })
}

impl Calls for Replies {
    fn calls(&self) -> &[Call] {
        &self.calls
    }
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    timestamp: Option<String>,
    message: Option<Message>,
    usage: Option<Usage>,
    /// A `usage` entry's: the model it called.
    provider: Option<String>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    role: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    /// The model that answered, as the provider said.
    #[serde(rename = "responseModel")]
    response_model: Option<String>,
    usage: Option<Usage>,
    /// In ms.
    timestamp: Option<i64>,
    /// A tool's result's, the tool's own: the subagent extension's runs.
    details: Option<Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input: u64,
    output: u64,
    #[serde(rename = "cacheRead")]
    cache_read: u64,
    #[serde(rename = "cacheWrite")]
    cache_write: u64,
    cost: Option<Cost>,
}

#[derive(Default, Deserialize)]
struct Cost {
    total: Option<f64>,
}

/// Every session file pi keeps, each with its conversation's id.
pub fn files(sessions: &Path) -> Vec<(PathBuf, String)> {
    super::conversations::session_files(sessions)
        .filter_map(|p| {
            let id = super::conversations::id_of(&p)?.to_string();
            Some((p, id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Jsonl;
    use crate::tokens::Spending;

    #[test]
    fn each_reply_and_warming_is_a_call_with_its_cost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let lines = [
            r#"{"type":"session","id":"s","timestamp":"2026-10-25T10:00:00.000Z"}"#,
            r#"{"type":"message","id":"u","timestamp":"2026-10-25T10:00:01.000Z","message":{"role":"user","content":"what \"usage\"?"}}"#,
            r#"{"type":"message","id":"a","timestamp":"2026-10-25T10:00:02.000Z","message":{"role":"assistant","provider":"anthropic","model":"claude-opus-5-5","usage":{"input":10,"output":5,"cacheRead":100,"cacheWrite":20,"cost":{"total":0.25}},"timestamp":1792922402000}}"#,
            // Cache warming of its own model (pi 1.3's `appendUsage`), and,
            // naming none, of the last reply's.
            r#"{"type":"usage","id":"w","timestamp":"2026-10-25T10:05:00.000Z","kind":"cache_warm","provider":"openai","model":"gpt-6.1-sol","usage":{"output":0,"cacheRead":1000000,"cost":{"total":0.1}}}"#,
            r#"{"type":"usage","kind":"cache_warm","timestamp":"2026-10-25T10:05:30.000Z","usage":{"output":0,"cacheRead":50,"cost":{"total":0.125}}}"#,
            r#"{"type":"message","id":"b","timestamp":"2026-10-25T10:06:00.000Z","message":{"role":"assistant","provider":"vllm","model":"qwen3","responseModel":"qwen3.1","usage":{"input":7,"output":1,"cost":{"total":0}}}}"#,
            // Summaries, each a call of its own (pi 1.3's `appendCompaction`,
            // `branchWithSummary`).
            r#"{"type":"compaction","id":"c","parentId":"b","timestamp":"2026-10-25T10:07:00.000Z","summary":"…","firstKeptEntryId":"b","tokensBefore":9,"usage":{"input":2000,"output":3,"cacheRead":0,"cacheWrite":0,"cost":{"total":0}}}"#,
            r#"{"type":"branch_summary","id":"d","parentId":"a","timestamp":"2026-10-25T10:08:00.000Z","fromId":"c","summary":"…","usage":{"input":40,"output":2}}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut replies = Jsonl::<Replies>::new(&path);
        replies.update().unwrap();
        let calls = replies.calls();
        let read: Vec<(&str, u64, Option<f64>)> = calls
            .iter()
            .map(|c| (c.model.as_str(), c.counts.total(), c.cost))
            .collect();
        assert_eq!(
            read,
            [
                ("anthropic/claude-opus-5-5", 135, Some(0.25)),
                ("openai/gpt-6.1-sol", 1_000_000, Some(0.1)),
                ("anthropic/claude-opus-5-5", 50, Some(0.125)),
                // The model that answered, as the provider said.
                ("vllm/qwen3.1", 8, Some(0.0)),
                ("vllm/qwen3.1", 2003, Some(0.0)),
                ("vllm/qwen3.1", 42, None),
            ]
        );
        // Each by its entry, which a fork copies as it is.
        assert_eq!(calls[0].id, "pi:a@2026-10-25T10:00:02.000Z");
        // The reply's own time, else its entry's.
        assert_eq!(calls[0].at, 1_792_922_402_000);
        assert_eq!(calls[3].at, iso_ms("2026-10-25T10:06:00.000Z").unwrap());
    }

    #[test]
    fn a_subagents_replies_kept_in_its_tools_result_are_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let reply = |model: &str, output: u64| {
            serde_json::json!({"role": "assistant", "provider": "anthropic", "model": model,
                "usage": {"input": 100, "output": output, "cost": {"total": 0.5}}, "timestamp": 1_792_922_500_000_i64})
        };
        // As pi's subagent extension keeps its runs: two, the second with
        // a subagent of its own.
        let nested = serde_json::json!({"role": "toolResult", "toolName": "subagent",
            "details": {"results": [{"messages": [reply("claude-haiku-5-5", 3)]}]}});
        let result = serde_json::json!({"type": "message", "id": "t", "timestamp": "2026-10-25T10:01:00.000Z",
        "message": {"role": "toolResult", "toolName": "subagent", "content": [], "details": {"mode": "parallel",
            "results": [
                {"agent": "scout", "usage": {"input": 100, "output": 7}, "messages": [
                    {"role": "user", "content": "find it"}, reply("claude-sonnet-5-5", 7)]},
                {"agent": "worker", "messages": [reply("claude-opus-5-5", 9), nested,
                    // One of a shape it isn't, among them: only it is left out.
                    {"role": "assistant", "usage": {"input": -1}}, "not a message"]},
            ]}}});
        // Another tool's result, whatever its details, is no call; one that
        // says what it spent itself is.
        let other = serde_json::json!({"type": "message", "id": "b", "timestamp": "2026-10-25T10:02:00.000Z",
            "message": {"role": "toolResult", "toolName": "bash", "details": {"results": "usage"},
                "usage": {"input": 1000, "output": 500}}});
        std::fs::write(&path, format!("{result}\n{other}\n")).unwrap();
        let mut replies = Jsonl::<Replies>::new(&path);
        replies.update().unwrap();
        let read: Vec<(&str, &str, u64)> = replies
            .calls()
            .iter()
            .map(|c| (c.id.as_str(), c.model.as_str(), c.counts.output))
            .collect();
        assert_eq!(
            read,
            [
                (
                    "pi:t@2026-10-25T10:01:00.000Z/0/1",
                    "anthropic/claude-sonnet-5-5",
                    7
                ),
                (
                    "pi:t@2026-10-25T10:01:00.000Z/1/0",
                    "anthropic/claude-opus-5-5",
                    9
                ),
                (
                    "pi:t@2026-10-25T10:01:00.000Z/1/1/0/0",
                    "anthropic/claude-haiku-5-5",
                    3
                ),
                // Of the model last replying (none here).
                ("pi:b@2026-10-25T10:02:00.000Z/u", "unknown", 500),
            ]
        );
        assert_eq!(replies.calls()[0].cost, Some(0.5));
    }
}
