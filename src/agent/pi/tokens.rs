//! What pi spent, call by call, from its session files: each reply's usage,
//! with its provider, model (the one that answered, if the provider said)
//! and what pi says it cost, and the model calls outside the conversation:
//! cache warming (`usage` entries, with their own model), and the summaries
//! of a compaction or of a branch left, of the model last replying.
//! Every branch of the tree counts: it was spent either way. A fork copies
//! the entries it was forked from as they were, their ids and times too,
//! which name the calls, so a copy is one call.

use std::path::{Path, PathBuf};

use serde::Deserialize;

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
        let (usage, at, model) = match entry.kind.as_deref() {
            Some("message") => {
                let Some(message) = entry
                    .message
                    .filter(|m| m.role.as_deref() == Some("assistant"))
                else {
                    return;
                };
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
        let (Some(usage), Some(at)) = (usage, at) else {
            return;
        };
        self.calls.push(Call {
            id: format!(
                "pi:{}@{}",
                entry.id.as_deref().unwrap_or(""),
                entry.timestamp.as_deref().unwrap_or("")
            ),
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
}
