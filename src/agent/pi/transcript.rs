//! Pi's session file: JSONL entries forming a tree (branches stay in the file),
//! each message with its usage and cost. Whatever can't be read is left out.
//! A fork (`--fork`) begins with its header, naming the session it came from,
//! then a copy of that one's entries as they were, dated then: those spent
//! nothing in it.

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::stats::{AgentStats, Jsonl, Lines, Prompts, Tally, Text, leading_type};
use crate::util::iso_ms;

/// Reads a pi session file as it grows.
pub type PiTranscript = Jsonl<Totals>;

/// A pi session file's running totals.
#[derive(Debug, Default)]
pub struct Totals {
    prompts: Prompts,
    output: u64,
    /// The tokens in context after the last reply; unknown after a compaction
    /// until the next one.
    context: Option<u64>,
    /// What every model call cost, on all branches: it was spent either way.
    cost: f64,
    costed: bool,
    /// A fork's: when it was made (ms), what's older being the copy.
    forked_at: Option<i64>,
}

impl Totals {
    fn count(&mut self, usage: &Usage) {
        self.output += usage.output;
        if let Some(total) = usage.cost.as_ref().and_then(|c| c.total) {
            self.cost += total;
            self.costed = true;
        }
    }

    /// What a tool spent: what its result says (an extension's own model
    /// calls), else a subagent's replies kept in it (pi's subagent
    /// extension), its subagents' in turn.
    fn tool_result(&mut self, message: &Message) {
        if let Some(usage) = &message.usage {
            self.count(usage);
            return;
        }
        let runs = message.details.as_ref().and_then(|d| d.get("results"));
        for run in runs.and_then(Value::as_array).into_iter().flatten() {
            let messages = run.get("messages").and_then(Value::as_array);
            for message in messages.into_iter().flatten() {
                let Ok(message) = Message::deserialize(message) else {
                    continue;
                };
                match message.role.as_deref() {
                    Some("assistant") => {
                        if let Some(usage) = &message.usage {
                            self.count(usage);
                        }
                    }
                    Some("toolResult") => self.tool_result(&message),
                    _ => {}
                }
            }
        }
    }
}

impl Lines for Totals {
    fn line(&mut self, line: &[u8]) {
        if !wanted(line) {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let at = entry.timestamp.as_deref().and_then(iso_ms);
        // In a fork, the copy: in context, but neither prompted nor spent here.
        let copied = self
            .forked_at
            .is_some_and(|made| at.is_some_and(|at| at <= made));
        match entry.kind.as_deref() {
            Some("session") => {
                if entry.parent_session.is_some() {
                    self.forked_at = at;
                }
            }
            Some("message") => {
                let message = entry.message.unwrap_or_default();
                match message.role.as_deref() {
                    Some("assistant") => {
                        if let Some(usage) = message.usage {
                            if !copied {
                                self.count(&usage);
                            }
                            let context =
                                usage.input + usage.output + usage.cache_read + usage.cache_write;
                            if context > 0 {
                                self.context = Some(context);
                            }
                        }
                    }
                    Some("user") if !copied => {
                        if let Some(Text(Some(text))) = message.content
                            && !text.trim().is_empty()
                        {
                            self.prompts.push(text);
                        }
                    }
                    Some("toolResult") if !copied => self.tool_result(&message),
                    _ => {}
                }
            }
            // Model calls outside the conversation: cache warming, a
            // compaction's or a branch left's summary.
            Some("usage" | "branch_summary") if !copied => {
                if let Some(usage) = entry.usage {
                    self.count(&usage);
                }
            }
            Some("compaction") => {
                self.context = None;
                if let Some(usage) = entry.usage.filter(|_| !copied) {
                    self.count(&usage);
                }
            }
            _ => {}
        }
    }
}

impl Tally for Totals {
    /// The context window isn't in the file; pi reports it (see
    /// `Report::context_window`).
    fn stats(&self, _model: Option<&str>) -> AgentStats {
        AgentStats {
            prompts: Some(self.prompts.count),
            context_used: self.context,
            output_tokens: (self.output > 0).then_some(self.output),
            cost_usd: self.costed.then_some(self.cost),
            ..AgentStats::default()
        }
    }

    fn prompts(&self) -> &Prompts {
        &self.prompts
    }
}

/// What roster reads of an entry; the rest of it is skipped unread.
#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    message: Option<Message>,
    usage: Option<Usage>,
    /// A fork's header's: the session it came from.
    #[serde(rename = "parentSession")]
    parent_session: Option<IgnoredAny>,
}

#[derive(Default, Deserialize)]
struct Message {
    role: Option<String>,
    content: Option<Text>,
    usage: Option<Usage>,
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

/// Whether a line can be one roster reads. Pi writes `type` first: the
/// entries roster doesn't read are told apart without parsing them, and so are
/// messages whose role comes early on the line (tool results and the system
/// prompt are most of a long session).
fn wanted(line: &[u8]) -> bool {
    match leading_type(line) {
        None => true,
        Some(b"usage" | b"compaction" | b"branch_summary" | b"session") => true,
        Some(b"message") => {
            let head = &line[..line.len().min(400)];
            match memchr::memmem::find(head, br#""role":""#) {
                Some(i) => {
                    let role = &head[i + br#""role":""#.len()..];
                    role.starts_with(b"user\"")
                        || role.starts_with(b"assistant\"")
                        // A tool's, if it spent something: most don't.
                        || (role.starts_with(b"toolResult\"")
                            && memchr::memmem::find(line, br#""usage""#).is_some())
                }
                None => true,
            }
        }
        Some(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Transcript;

    fn message(role: &str, rest: serde_json::Value) -> String {
        let mut message = serde_json::json!({"role": role});
        message
            .as_object_mut()
            .unwrap()
            .extend(rest.as_object().unwrap().clone());
        format!(
            "{}\n",
            serde_json::json!({"type": "message", "id": "a1", "parentId": null, "timestamp": "t", "message": message})
        )
    }

    fn prompt(text: &str) -> String {
        message(
            "user",
            serde_json::json!({"content": [{"type": "text", "text": text}, {"type": "image", "data": "…"}]}),
        )
    }

    fn reply(output: u64, cache_read: u64, cost: f64) -> String {
        message(
            "assistant",
            serde_json::json!({"content": [{"type": "text", "text": "done"}], "usage": {
                "input": 2, "output": output, "cacheRead": cache_read, "cacheWrite": 10,
                "cost": {"total": cost}}}),
        )
    }

    #[test]
    fn a_forks_copy_spends_nothing_of_any_kind_read_as_pi_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // As pi writes them: `type` first, so the quick test of each line
        // sees its kind.
        let lines = [
            r#"{"type":"session","version":3,"id":"f","timestamp":"2026-10-25T11:00:00.000Z","parentSession":"/s/p.jsonl"}"#,
            // The copy: warming, a tool's own spend, a reply at the fork's
            // very moment.
            r#"{"type":"usage","id":"w","timestamp":"2026-10-25T10:00:00.000Z","usage":{"input":0,"output":0,"cacheRead":900,"cost":{"total":0.5}}}"#,
            r#"{"type":"message","id":"t","timestamp":"2026-10-25T10:01:00.000Z","message":{"role":"toolResult","usage":{"input":1,"output":30,"cost":{"total":0.5}}}}"#,
            r#"{"type":"message","id":"b","timestamp":"2026-10-25T11:00:00.000Z","message":{"role":"assistant","usage":{"input":1,"output":40,"cost":{"total":0.5}}}}"#,
            // Its own: a tool's spend, and a compaction's summary.
            r#"{"type":"message","id":"u","timestamp":"2026-10-25T12:00:00.000Z","message":{"role":"toolResult","usage":{"input":1,"output":3,"cost":{"total":0.125}}}}"#,
            r#"{"type":"compaction","id":"c","timestamp":"2026-10-25T12:01:00.000Z","summary":"…","usage":{"input":50,"output":2,"cost":{"total":0.0625}}}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut t = PiTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!((s.output_tokens, s.cost_usd), (Some(5), Some(0.1875)));
    }

    #[test]
    fn a_fork_counts_what_it_spent_itself_and_a_tools_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let at = |line: String, ts: &str| {
            line.replacen("\"timestamp\":\"t\"", &format!("\"timestamp\":\"{ts}\""), 1)
        };
        let (before, after) = ("2026-10-25T10:00:00.000Z", "2026-10-25T12:00:00.000Z");
        let mut text = format!(
            "{}\n",
            serde_json::json!({"type": "session", "version": 3, "id": "f", "timestamp": "2026-10-25T11:00:00.000Z",
                "parentSession": "/s/p.jsonl"})
        );
        // The copy of the session it came from, dated then.
        text += &at(prompt("first, in the parent"), before);
        text += &at(reply(100, 4000, 0.5), before);
        // Its own.
        text += &at(prompt("then, in the fork"), after);
        text += &at(reply(7, 5000, 0.25), after);
        // A tool's result: what an extension's own call spent, and pi's
        // subagent extension's replies.
        text += &at(
            message(
                "toolResult",
                serde_json::json!({"usage": {"input": 1, "output": 3, "cost": {"total": 0.125}}}),
            ),
            after,
        );
        text += &at(
            message(
                "toolResult",
                serde_json::json!({"content": [], "details": {"results": [{"messages": [
                {"role": "assistant", "usage": {"input": 1, "output": 2, "cost": {"total": 0.0625}}}]}]}}),
            ),
            after,
        );
        std::fs::write(&path, text).unwrap();
        let mut t = PiTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.cost_usd, s.context_used),
            (Some(1), Some(12), Some(0.4375), Some(5019))
        );
    }

    #[test]
    fn a_forks_copied_summaries_spend_nothing_and_its_own_do() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // As pi 1.4.0 writes them.
        let lines = [
            r#"{"type":"session","version":3,"id":"f","timestamp":"2026-10-25T11:00:00.000Z","cwd":"/w","parentSession":"/s/p.jsonl"}"#,
            // The copy: a compaction's summary and a branch left's.
            r#"{"type":"compaction","id":"c","parentId":"a","timestamp":"2026-10-25T10:30:00.000Z","summary":"…","firstKeptEntryId":"a","tokensBefore":9000,"usage":{"input":50,"output":60,"cost":{"total":1.0}}}"#,
            r#"{"type":"branch_summary","id":"g","parentId":"a","timestamp":"2026-10-25T10:40:00.000Z","fromId":"d","summary":"…","usage":{"input":50,"output":70,"cost":{"total":1.0}}}"#,
            // Its own: a branch left's summary, and a reply.
            r#"{"type":"branch_summary","id":"h","parentId":"c","timestamp":"2026-10-25T12:00:00.000Z","fromId":"e","summary":"…","usage":{"input":40,"output":4,"cost":{"total":0.25}}}"#,
            r#"{"type":"message","id":"r","parentId":"h","timestamp":"2026-10-25T12:01:00.000Z","message":{"role":"assistant","content":[],"usage":{"input":1,"output":3,"cacheRead":100,"cacheWrite":0,"cost":{"total":0.125}}}}"#,
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut t = PiTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.output_tokens, s.cost_usd, s.context_used),
            (Some(7), Some(0.375), Some(104))
        );
    }

    #[test]
    fn a_subagents_tools_spend_counts_however_deep() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        // Pi's subagent extension's run: its replies, a tool's own call, and
        // a subagent of its, with its replies.
        let run = serde_json::json!({"content": [], "details": {"results": [{"messages": [
            {"role": "assistant", "usage": {"input": 1, "output": 2, "cost": {"total": 0.0625}}},
            {"role": "toolResult", "usage": {"input": 1, "output": 3, "cost": {"total": 0.125}}},
            {"role": "toolResult", "content": [], "details": {"results": [{"messages": [
                {"role": "assistant", "usage": {"input": 1, "output": 5, "cost": {"total": 0.25}}}]}]}},
        ]}]}});
        std::fs::write(&path, message("toolResult", run)).unwrap();
        let mut t = PiTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!((s.output_tokens, s.cost_usd), (Some(10), Some(0.4375)));
    }

    #[test]
    fn totals_follow_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut text = r#"{"type":"session","version":3,"id":"c-1","cwd":"/p"}"#.to_string() + "\n";
        text += &prompt("fix the tests");
        text += &reply(500, 1000, 0.25);
        text += &message(
            "toolResult",
            serde_json::json!({"content": [{"type": "text", "text": "not a prompt"}]}),
        );
        text += r#"{"type":"model_change","id":"b","provider":"anthropic","modelId":"x"}"#;
        text += "\n";
        // Not prompts either, as pi 1.4.0 writes them: an extension's message
        // (a background subagent's result), and a command run with `!`.
        text += r#"{"type":"custom_message","customType":"subagent-result","content":"Subagent #1 finished","display":true,"id":"e","parentId":"b","timestamp":"t"}"#;
        text += "\n";
        text += &message(
            "bashExecution",
            serde_json::json!({"command": "ls", "output": "a\n", "exitCode": 0, "cancelled": false}),
        );
        std::fs::write(&path, &text).unwrap();
        let mut t = PiTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_used, s.cost_usd),
            (Some(1), Some(500), Some(1512), Some(0.25))
        );
        assert_eq!(
            (s.context_limit, s.lines_added, s.partial),
            (None, None, false)
        );

        // More later, the last line still being written; a usage entry
        // counts, and a compaction leaves the context unknown.
        let more = prompt("now add a retry")
            + &reply(300, 5000, 0.5)
            + r#"{"type":"usage","kind":"cache_warm","usage":{"output":0,"cacheRead":50,"cost":{"total":0.125}}}"#
            + "\n"
            + r#"{"type":"compaction","id":"c","summary":"…"}"#
            + "\n";
        let half = reply(100, 6000, 0.125);
        let (a, b) = half.split_at(30);
        std::fs::write(&path, format!("{text}{more}{a}")).unwrap();
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_used, s.cost_usd),
            (Some(2), Some(800), None, Some(0.875))
        );
        std::fs::write(&path, format!("{text}{more}{a}{b}")).unwrap();
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!((s.context_used, s.cost_usd), (Some(6112), Some(1.0)));
        assert_eq!(t.recent_prompts(), ["fix the tests", "now add a retry"]);
    }

    #[test]
    fn entries_not_read_are_skipped_unparsed() {
        assert!(!wanted(br#"{"type":"custom","data":{}}"#));
        assert!(!wanted(
            br#"{"type":"message","id":"a","message":{"role":"toolResult","content":[]}}"#
        ));
        assert!(wanted(
            br#"{"type":"message","id":"a","message":{"role":"user"}}"#
        ));
        assert!(wanted(br#"{"type":"usage","usage":{}}"#));
        assert!(wanted(br#"{"id":"a","type":"message"}"#));
    }
}
