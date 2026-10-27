//! What codex spent, call by call, from its rollouts (`sessions/`, and
//! `archived_sessions/` once archived): since 0.163 at the latest a
//! `token_usage_record` for each model call, then the `token_count` after it;
//! before, only the count, with the call's counts and the conversation's
//! total. A conversation resumed in a newer codex has both, the older calls
//! counted only. Codex counts cached input as input; roster counts them
//! apart. The model is the one the turn was given (`turn_context`).
//!
//! A fork (and a subagent, forked from its parent) starts with a copy of
//! the conversation it was forked from (since 0.137), written as it's made,
//! every line dated then: its counts and records too. That isn't spent
//! again, so it's skipped, from the copy's own `session_meta` up to the
//! first turn of the fork's own: a turn's id is a UUIDv7, made when the
//! turn was, so the copied ones are older than the fork, and one that
//! can't be dated (an older codex's, or none) is the copy's. An older
//! fork, with no copy, has nothing skipped.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::stats::Lines;
use crate::tokens::{Call, Calls, Counts};
use crate::util::iso_ms;

#[derive(Debug, Default)]
pub struct ModelCalls {
    calls: Vec<Call>,
    /// A call was recorded since the last count: the count is that call's.
    recorded: bool,
    /// The conversation's total at the last count, which codex also writes
    /// again without a call in between.
    total: Option<u64>,
    model: Option<String>,
    /// The rollout's own conversation, from its first `session_meta`; the
    /// copy of another's comes with that one's.
    own: Option<String>,
    met: bool,
    /// In a fork, when it was made (ms).
    forked: Option<i64>,
    /// While what's read is the copy, when the fork was made (ms).
    copying: Option<i64>,
    /// The copy began (once only).
    copied: bool,
}

impl Lines for ModelCalls {
    fn line(&mut self, line: &[u8]) {
        const MARKS: [&[u8]; 5] = [
            br#""session_meta""#,
            br#""turn_context""#,
            br#""task_started""#,
            br#""token_usage_record""#,
            br#""token_count""#,
        ];
        if !MARKS
            .iter()
            .any(|mark| memchr::memmem::find(line, mark).is_some())
        {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let Some(payload) = entry.payload else {
            return;
        };
        let stamp = entry.timestamp.unwrap_or_default();
        let at = iso_ms(&stamp);
        match (entry.kind.as_deref(), payload.kind.as_deref()) {
            (Some("session_meta"), _) if !self.met => {
                self.met = true;
                self.own = payload.id;
                if payload.forked_from_id.is_some() {
                    self.forked = Some(
                        payload
                            .timestamp
                            .as_deref()
                            .and_then(iso_ms)
                            .or(at)
                            .unwrap_or(0),
                    );
                }
            }
            // Another's, in a fork: the copy of the conversation it was
            // forked from begins.
            (Some("session_meta"), _) => {
                if self.forked.is_some() && !self.copied {
                    self.copying = self.forked;
                    self.copied = true;
                }
            }
            (Some("turn_context"), _) | (Some("event_msg"), Some("task_started")) => {
                // The fork's own first turn: the copy is over.
                if let (Some(made), Some(turn)) = (self.copying, payload.turn_id.as_deref())
                    && uuid_ms(turn).is_some_and(|t| t >= made)
                {
                    self.copying = None;
                }
                if let Some(model) = payload.model {
                    self.model = Some(model);
                }
            }
            (Some("token_usage_record"), _) => {
                self.recorded = true;
                // A record names its conversation: in a fork, the fork's own
                // counts, whether or not its turn could be dated.
                let copied = match payload.thread_id.as_deref() {
                    Some(thread) if self.forked.is_some() => Some(thread) != self.own.as_deref(),
                    _ => self.copying.is_some(),
                };
                if let (Some(usage), Some(at), false) = (payload.usage, at, copied) {
                    let id = match payload.response_id {
                        Some(response) => format!("codex:{response}"),
                        None => self.unnamed(&stamp, "record"),
                    };
                    let call = self.call(id, at, &usage);
                    self.calls.push(call);
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let Some(Info {
                    total: Some(total),
                    last: Some(last),
                }) = payload.info
                else {
                    return;
                };
                if self.total == Some(total.total_tokens) {
                    return;
                }
                self.total = Some(total.total_tokens);
                // The count of a call recorded already.
                if std::mem::take(&mut self.recorded) || self.copying.is_some() {
                    return;
                }
                if let Some(at) = at {
                    let id = self.unnamed(&stamp, &total.total_tokens.to_string());
                    let call = self.call(id, at, &last);
                    self.calls.push(call);
                }
            }
            _ => {}
        }
    }
}

impl ModelCalls {
    /// A call codex gives no id: by its conversation, when it was written
    /// and what else tells it apart, which a moved rollout keeps.
    fn unnamed(&self, stamp: &str, what: &str) -> String {
        format!("codex:{}@{stamp}:{what}", self.own.as_deref().unwrap_or(""))
    }

    fn call(&self, id: String, at: i64, usage: &Usage) -> Call {
        let cached = usage.cached_input_tokens + usage.cache_write_input_tokens;
        Call {
            id,
            at,
            model: self.model.clone().unwrap_or_else(|| "unknown".into()),
            counts: Counts {
                input: usage.input_tokens.saturating_sub(cached),
                cache_read: usage.cached_input_tokens,
                cache_write: usage.cache_write_input_tokens,
                cache_write_hour: 0,
                output: usage.output_tokens,
            },
            ..Call::default()
        }
    }
}

impl Calls for ModelCalls {
    fn calls(&self) -> &[Call] {
        &self.calls
    }
}

/// When a UUIDv7 was made, in ms (its first 48 bits); None for another kind.
pub(super) fn uuid_ms(id: &str) -> Option<i64> {
    let hex: String = id.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || hex.as_bytes()[12] != b'7' {
        return None;
    }
    i64::from_str_radix(&hex[..12], 16).ok()
}

#[derive(Deserialize)]
struct Entry {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    info: Option<Info>,
    /// A `session_meta`'s: its conversation, the one it was forked from,
    /// and when it was made.
    id: Option<String>,
    forked_from_id: Option<String>,
    timestamp: Option<String>,
    turn_id: Option<String>,
    /// A record's: the conversation and the response.
    thread_id: Option<String>,
    response_id: Option<String>,
}

#[derive(Deserialize)]
struct Info {
    #[serde(rename = "total_token_usage")]
    total: Option<Usage>,
    #[serde(rename = "last_token_usage")]
    last: Option<Usage>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    input_tokens: u64,
    cached_input_tokens: u64,
    cache_write_input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

/// Every rollout in codex's directory, each with its conversation's id.
pub fn files(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut found = super::conversations::files(&dir.join("sessions"), 1);
    found.extend(super::conversations::files(
        &dir.join("archived_sessions"),
        1,
    ));
    found
        .into_iter()
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
    use serde_json::json;

    fn read(lines: &[serde_json::Value]) -> Vec<Call> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, text).unwrap();
        let mut calls = Jsonl::<ModelCalls>::new(&path);
        calls.update().unwrap();
        calls.calls().to_vec()
    }

    fn usage(input: u64, cached: u64, output: u64, total: u64) -> serde_json::Value {
        json!({"input_tokens": input, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
            "output_tokens": output, "reasoning_output_tokens": 0, "total_tokens": total})
    }

    fn count(at: &str, total: u64, last: serde_json::Value) -> serde_json::Value {
        json!({"timestamp": at, "type": "event_msg", "payload": {"type": "token_count",
            "info": {"total_token_usage": {"total_tokens": total}, "last_token_usage": last}}})
    }

    fn context(model: &str) -> serde_json::Value {
        json!({"timestamp": "2026-10-25T10:00:00.000Z", "type": "turn_context", "payload": {"model": model}})
    }

    #[test]
    fn each_record_is_a_call_of_the_turns_model() {
        let record = |at: &str, u| json!({"timestamp": at, "type": "token_usage_record", "payload": {"usage": u}});
        let calls = read(&[
            context("gpt-6.1-sol"),
            record("2026-10-25T10:00:01.000Z", usage(1000, 800, 50, 1050)),
            count("2026-10-25T10:00:01.001Z", 1050, usage(1000, 800, 50, 1050)),
            context("gpt-6-luna"),
            record("2026-10-25T10:00:09.000Z", usage(2000, 1500, 70, 2070)),
        ]);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].model, "gpt-6.1-sol");
        assert_eq!(
            calls[0].counts,
            Counts {
                input: 200,
                cache_read: 800,
                cache_write: 0,
                cache_write_hour: 0,
                output: 50
            }
        );
        assert_eq!(calls[1].model, "gpt-6-luna");
        assert_eq!(calls[1].at, iso_ms("2026-10-25T10:00:09.000Z").unwrap());
    }

    #[test]
    fn without_records_each_new_count_is_a_call() {
        let calls = read(&[
            context("gpt-5.5"),
            count("2026-04-13T18:11:57.000Z", 100, usage(90, 0, 10, 100)),
            // Written again without a call between: the same.
            count("2026-04-13T18:11:58.000Z", 100, usage(90, 0, 10, 100)),
            count("2026-04-13T18:12:30.000Z", 250, usage(140, 100, 10, 150)),
            json!({"timestamp": "2026-04-13T18:12:31.000Z", "type": "event_msg",
                "payload": {"type": "token_count", "info": null}}),
        ]);
        let outputs: Vec<(u64, u64)> = calls
            .iter()
            .map(|c| (c.counts.input, c.counts.cache_read))
            .collect();
        assert_eq!(outputs, [(90, 0), (40, 100)]);
    }

    fn record(at: &str, thread: &str, response: &str, u: serde_json::Value) -> serde_json::Value {
        json!({"timestamp": at, "type": "token_usage_record",
            "payload": {"thread_id": thread, "response_id": response, "usage": u}})
    }

    #[test]
    fn a_conversation_resumed_in_a_newer_codex_keeps_its_older_calls() {
        let calls = read(&[
            json!({"timestamp": "2026-04-13T18:00:00.000Z", "type": "session_meta", "payload": {"id": "c1"}}),
            context("gpt-5.5"),
            count("2026-04-13T18:11:57.000Z", 1000, usage(1000, 0, 0, 1000)),
            // Resumed in 0.163: each call recorded, then counted.
            context("gpt-6.1-sol"),
            record(
                "2026-10-25T10:00:01.000Z",
                "c1",
                "resp_1",
                usage(2000, 0, 0, 2000),
            ),
            count("2026-10-25T10:00:01.001Z", 3000, usage(2000, 0, 0, 2000)),
        ]);
        let read: Vec<(&str, u64)> = calls
            .iter()
            .map(|c| (c.id.as_str(), c.counts.input))
            .collect();
        assert_eq!(
            read,
            [
                ("codex:c1@2026-04-13T18:11:57.000Z:1000", 1000),
                ("codex:resp_1", 2000)
            ]
        );
    }

    /// A UUIDv7 made at `ms`, as codex names turns.
    fn turn(ms: i64) -> String {
        let hex = format!("{ms:012x}");
        format!("{}-{}-7000-8000-000000000000", &hex[..8], &hex[8..])
    }

    #[test]
    fn a_forks_copy_of_the_conversation_it_was_forked_from_isnt_spent_again() {
        let made = iso_ms("2026-06-05T17:08:53.197Z").unwrap();
        let task = |at: &str, turn: String| json!({"timestamp": at, "type": "event_msg", "payload": {"type": "task_started", "turn_id": turn}});
        let now = "2026-06-05T17:08:53.240Z";
        let calls = read(&[
            json!({"timestamp": now, "type": "session_meta", "payload": {"id": "fork",
                "forked_from_id": "parent", "timestamp": "2026-06-05T17:08:53.197Z"}}),
            // The copy, dated as it was made: the parent's meta, turns and
            // calls, old and new kinds.
            json!({"timestamp": now, "type": "session_meta", "payload": {"id": "parent"}}),
            task(now, turn(made - 950_000_000)),
            context("gpt-5.5"),
            count(now, 500, usage(500, 0, 0, 500)),
            record(now, "parent", "resp_p", usage(700, 0, 0, 700)),
            count(now, 1200, usage(700, 0, 0, 700)),
            task(now, turn(made - 5_000)),
            count(now, 1300, usage(100, 0, 0, 100)),
            // Its own first turn, and a call in it.
            task("2026-06-05T17:08:53.291Z", turn(made + 91)),
            context("gpt-6.1-sol"),
            record(
                "2026-06-05T17:08:57.000Z",
                "fork",
                "resp_f",
                usage(40, 0, 0, 40),
            ),
            count("2026-06-05T17:08:57.001Z", 1340, usage(40, 0, 0, 40)),
            // Codex itself counting the parent's call it shows the fork, after.
            record(
                "2026-06-05T17:08:58.000Z",
                "parent",
                "resp_p",
                usage(700, 0, 0, 700),
            ),
        ]);
        let read: Vec<(&str, &str, u64)> = calls
            .iter()
            .map(|c| (c.id.as_str(), c.model.as_str(), c.counts.input))
            .collect();
        assert_eq!(read, [("codex:resp_f", "gpt-6.1-sol", 40)]);
        assert_eq!(uuid_ms(&turn(made)), Some(made));
        assert_eq!(uuid_ms("not-a-uuid"), None);
    }

    #[test]
    fn a_copy_whose_turns_cant_be_dated_is_skipped_and_an_older_fork_isnt() {
        let made = iso_ms("2026-06-05T17:08:53.197Z").unwrap();
        let task = |turn: Option<String>| {
            json!({"timestamp": "2026-06-05T17:08:53.240Z",
            "type": "event_msg", "payload": {"type": "task_started", "turn_id": turn}})
        };
        let fork = json!({"timestamp": "2026-06-05T17:08:53.240Z", "type": "session_meta",
            "payload": {"id": "fork", "forked_from_id": "parent", "timestamp": "2026-06-05T17:08:53.197Z"}});
        // A parent from a codex whose turn ids weren't v7, copied.
        let calls = read(&[
            fork.clone(),
            json!({"timestamp": "2026-06-05T17:08:53.240Z", "type": "session_meta", "payload": {"id": "parent"}}),
            task(Some("6f9619ff-8b86-4011-b42d-00c04fc964ff".into())),
            count("2026-06-05T17:08:53.240Z", 500, usage(500, 0, 0, 500)),
            task(None),
            count("2026-06-05T17:08:53.241Z", 900, usage(400, 0, 0, 400)),
            task(Some(turn(made + 50))),
            count("2026-06-05T17:08:57.000Z", 940, usage(40, 0, 0, 40)),
        ]);
        let inputs: Vec<u64> = calls.iter().map(|c| c.counts.input).collect();
        assert_eq!(inputs, [40]);
        // The fork's own record, though its turn can't be dated, counts; a
        // session_meta after the copy doesn't begin another.
        let calls = read(&[
            fork.clone(),
            json!({"timestamp": "2026-06-05T17:08:53.240Z", "type": "session_meta", "payload": {"id": "parent"}}),
            task(None),
            record(
                "2026-06-05T17:08:57.000Z",
                "fork",
                "resp_f",
                usage(40, 0, 0, 40),
            ),
            count("2026-06-05T17:08:57.001Z", 8000, usage(40, 0, 0, 40)),
            task(Some(turn(made + 50))),
            json!({"timestamp": "2026-06-05T17:09:00.000Z", "type": "session_meta", "payload": {"id": "other"}}),
            count("2026-06-05T17:09:01.000Z", 9000, usage(30, 0, 0, 30)),
        ]);
        let inputs: Vec<u64> = calls.iter().map(|c| c.counts.input).collect();
        assert_eq!(inputs, [40, 30]);
        // A fork with no copy (before 0.137), its turns undated: all its own.
        let calls = read(&[
            fork,
            task(None),
            count("2026-06-05T17:09:00.000Z", 70, usage(70, 0, 0, 70)),
        ]);
        let inputs: Vec<u64> = calls.iter().map(|c| c.counts.input).collect();
        assert_eq!(inputs, [70]);
    }
}
