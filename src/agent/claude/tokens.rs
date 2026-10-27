//! What Claude spent, call by call: each reply's usage in its transcripts
//! (`<claude dir>/projects/<dir>/<id>.jsonl`, and its subagents' beside them
//! in `<id>/subagents/`, some in a directory beneath it). Claude writes a
//! reply once for each part of it (its thinking, its text, each tool it
//! calls), its usage with each as of then, so a reply counts once, each
//! count as large as it was written. A fork (`--fork-session`) copies the
//! replies with their ids, which name the calls, so a copy is one call.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::stats::{Lines, leading_type};
use crate::tokens::{Call, Calls, Counts, Served};
use crate::util::iso_ms;

/// The replies in a transcript, as it grows.
#[derive(Debug, Default)]
pub struct Replies {
    calls: Vec<Call>,
    /// Where each reply is in `calls`, by its id.
    by_id: HashMap<String, usize>,
}

impl Lines for Replies {
    fn line(&mut self, line: &[u8]) {
        if leading_type(line).is_some_and(|kind| kind != b"assistant")
            || memchr::memmem::find(line, br#""usage""#).is_none()
        {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let (Some("assistant"), Some(message)) = (entry.kind.as_deref(), entry.message) else {
            return;
        };
        let (Some(usage), Some(model), Some(at)) = (
            message.usage,
            message.model,
            entry.timestamp.as_deref().and_then(iso_ms),
        ) else {
            return;
        };
        // What Claude says itself (an error, an interruption), not a model.
        if model == "<synthetic>" {
            return;
        }
        let Some(id) = message.id.clone().or(entry.uuid) else {
            return;
        };
        let n = |count: Option<u64>| count.unwrap_or(0);
        let call = Call {
            id: format!("claude:{id}"),
            at,
            model,
            counts: Counts {
                input: n(usage.input_tokens),
                cache_read: n(usage.cache_read_input_tokens),
                cache_write: n(usage.cache_creation_input_tokens),
                cache_write_hour: usage
                    .cache_creation
                    .map_or(0, |c| n(c.ephemeral_1h_input_tokens)),
                output: n(usage.output_tokens),
            },
            cost: None,
            served: Served {
                searches: usage
                    .server_tool_use
                    .map_or(0, |t| n(t.web_search_requests)),
                us_only: usage.inference_geo.as_deref() == Some("us"),
                fast: usage.speed.as_deref() == Some("fast"),
            },
        };
        match message.id {
            Some(id) => match self.by_id.get(&id) {
                Some(&i) => {
                    let was = &mut self.calls[i];
                    let (c, more) = (&mut was.counts, &call.counts);
                    c.input = c.input.max(more.input);
                    c.cache_read = c.cache_read.max(more.cache_read);
                    c.cache_write = c.cache_write.max(more.cache_write);
                    c.cache_write_hour = c.cache_write_hour.max(more.cache_write_hour);
                    c.output = c.output.max(more.output);
                    // Written with a later part, as Claude merges them.
                    let (s, more) = (&mut was.served, &call.served);
                    s.searches = s.searches.max(more.searches);
                    s.fast |= more.fast;
                    s.us_only |= more.us_only;
                }
                None => {
                    self.by_id.insert(id, self.calls.len());
                    self.calls.push(call);
                }
            },
            None => self.calls.push(call),
        }
    }
}

impl Calls for Replies {
    fn calls(&self) -> &[Call] {
        &self.calls
    }
}

#[derive(Deserialize)]
struct Entry<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    uuid: Option<String>,
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

/// Its counts may be null.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation: Option<CacheCreation>,
    server_tool_use: Option<ServerTools>,
    inference_geo: Option<String>,
    speed: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ServerTools {
    web_search_requests: Option<u64>,
}

/// Every transcript in `claude_dir`, each with its conversation's id: its
/// own, or for a subagent's, its parent's.
pub fn files(claude_dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for project in fs::read_dir(claude_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let mut transcripts = Vec::new();
        jsonl(&project.path(), "", 0, &mut transcripts);
        for path in transcripts {
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let subagents = subagent_files(&project.path().join(&id).join("subagents"));
            found.extend(subagents.into_iter().map(|p| (p, id.clone())));
            found.push((path, id));
        }
    }
    found
}

/// The subagents' transcripts in a conversation's `subagents` directory.
pub fn subagent_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    jsonl(dir, "agent-", 3, &mut found);
    found
}

/// The JSONL files named `prefix…` in `dir` (one linked there too), and in
/// its directories `depth` deep (not linked ones, which could loop).
fn jsonl(dir: &Path, prefix: &str, depth: usize, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            if depth > 0 {
                jsonl(&path, prefix, depth - 1, found);
            }
        } else if path.extension().is_some_and(|e| e == "jsonl")
            && entry.file_name().to_string_lossy().starts_with(prefix)
            && path.is_file()
        {
            found.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Jsonl;
    use crate::tokens::Spending;

    #[test]
    fn a_reply_counts_once_as_last_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let reply = |id: &str, at: &str, output: u64| {
            serde_json::json!({"parentUuid": "p", "type": "assistant", "timestamp": at,
                "message": {"id": id, "model": "claude-opus-5-5", "content": [{"type": "text", "text": "\"usage\""}],
                    "usage": {"input_tokens": 2, "cache_creation_input_tokens": 300, "cache_read_input_tokens": 4000,
                        "output_tokens": output, "cache_creation": {"ephemeral_1h_input_tokens": 300, "ephemeral_5m_input_tokens": 0}}}})
            .to_string()
        };
        let lines = [
            reply("m1", "2026-10-25T18:00:00.000Z", 10),
            reply("m1", "2026-10-25T18:00:02.000Z", 99),
            r#"{"type":"user","message":{"role":"user","content":"\"usage\""}}"#.into(),
            reply("m2", "2026-10-25T18:01:00.000Z", 5),
            serde_json::json!({"type": "assistant", "timestamp": "2026-10-25T18:02:00.000Z",
                "message": {"id": "m3", "model": "<synthetic>", "usage": {"output_tokens": 0}}})
            .to_string(),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut replies = Jsonl::<Replies>::new(&path);
        replies.update().unwrap();
        let calls = replies.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].at, iso_ms("2026-10-25T18:00:00.000Z").unwrap());
        assert_eq!(
            calls[0].counts,
            Counts {
                input: 2,
                cache_read: 4000,
                cache_write: 300,
                cache_write_hour: 300,
                output: 99
            }
        );
        assert_eq!(calls[0].model, "claude-opus-5-5");
        assert_eq!(calls[0].id, "claude:m1");
        assert_eq!(calls[1].counts.output, 5);
    }

    #[test]
    fn a_reply_keeps_its_largest_counts_null_ones_none_and_how_it_was_served() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let reply = |output: serde_json::Value, extra: serde_json::Value| {
            let mut usage = serde_json::json!({"input_tokens": 100, "output_tokens": output,
                "cache_read_input_tokens": null, "cache_creation_input_tokens": null});
            usage
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::json!({"type": "assistant", "timestamp": "2026-10-25T18:00:00.000Z",
                "message": {"id": "m1", "model": "claude-opus-5-5", "usage": usage}})
            .to_string()
        };
        let lines = [
            reply(serde_json::json!(1), serde_json::json!({})),
            reply(
                serde_json::json!(40),
                serde_json::json!({"speed": "fast", "inference_geo": "us",
                    "server_tool_use": {"web_search_requests": 2, "web_fetch_requests": 0}}),
            ),
            // Written again with less, or none: what it spent stays.
            reply(serde_json::json!(0), serde_json::json!({})),
            reply(serde_json::Value::Null, serde_json::json!({})),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut replies = Jsonl::<Replies>::new(&path);
        replies.update().unwrap();
        let calls = replies.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].counts.input, calls[0].counts.output), (100, 40));
        assert_eq!(
            calls[0].served,
            Served {
                searches: 2,
                us_only: true,
                fast: true
            }
        );
    }

    #[test]
    fn a_subagents_transcript_is_of_its_parents_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("projects/-home-me-repo");
        std::fs::create_dir_all(project.join("c1/subagents")).unwrap();
        std::fs::create_dir_all(project.join("c1/subagents/group/g1")).unwrap();
        for file in [
            "c1.jsonl",
            "c1/subagents/agent-a1.jsonl",
            "c1/subagents/agent-a1.meta.json",
            "c1/subagents/group/g1/journal.jsonl",
            "c1/subagents/group/g1/agent-w1.jsonl",
            "notes.txt",
        ] {
            std::fs::write(project.join(file), "").unwrap();
        }
        let mut found = files(dir.path());
        found.sort();
        let found: Vec<(String, &str)> = found
            .iter()
            .map(|(p, id)| {
                (
                    p.strip_prefix(&project).unwrap().display().to_string(),
                    id.as_str(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                ("c1/subagents/agent-a1.jsonl".to_string(), "c1"),
                ("c1/subagents/group/g1/agent-w1.jsonl".to_string(), "c1"),
                ("c1.jsonl".to_string(), "c1")
            ]
        );
    }
}
