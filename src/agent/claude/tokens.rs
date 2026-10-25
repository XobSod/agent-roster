//! What Claude spent, call by call: each reply's usage in its transcripts
//! (`<claude dir>/projects/<dir>/<id>.jsonl`, and its subagents' beside them
//! in `<id>/subagents/`). Claude writes a reply once for each part of it
//! (its thinking, its text, each tool it calls), its usage with each, so a
//! reply counts once, as last written. A fork (`--fork-session`) copies the
//! replies with their ids, which name the calls, so a copy is one call.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::stats::{Lines, leading_type};
use crate::tokens::{Call, Calls, Counts};
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
        let call = Call {
            id: format!("claude:{id}"),
            at,
            model,
            counts: Counts {
                input: usage.input_tokens,
                cache_read: usage.cache_read_input_tokens,
                cache_write: usage.cache_creation_input_tokens,
                cache_write_hour: usage
                    .cache_creation
                    .map_or(0, |c| c.ephemeral_1h_input_tokens),
                output: usage.output_tokens,
            },
            cost: None,
        };
        match message.id {
            Some(id) => match self.by_id.get(&id) {
                Some(&i) => {
                    let at = self.calls[i].at;
                    self.calls[i] = Call { at, ..call };
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

#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    cache_read_input_tokens: u64,
    cache_creation_input_tokens: u64,
    output_tokens: u64,
    cache_creation: Option<CacheCreation>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CacheCreation {
    ephemeral_1h_input_tokens: u64,
}

/// Every transcript in `claude_dir`, each with its conversation's id: its
/// own, or for a subagent's, its parent's.
pub fn files(claude_dir: &Path) -> Vec<(PathBuf, String)> {
    let jsonl = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect()
    };
    let mut found = Vec::new();
    for project in fs::read_dir(claude_dir.join("projects"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for path in jsonl(&project.path()) {
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let subagents = project.path().join(&id).join("subagents");
            found.extend(jsonl(&subagents).into_iter().map(|p| (p, id.clone())));
            found.push((path, id));
        }
    }
    found
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
    fn a_subagents_transcript_is_of_its_parents_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("projects/-home-me-repo");
        std::fs::create_dir_all(project.join("c1/subagents")).unwrap();
        for file in ["c1.jsonl", "c1/subagents/agent-a1.jsonl", "notes.txt"] {
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
                ("c1.jsonl".to_string(), "c1")
            ]
        );
    }
}
