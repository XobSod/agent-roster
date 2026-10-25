//! Pi's session file: JSONL entries forming a tree (branches stay in the file),
//! each message with its usage and cost. Whatever can't be read is left out.

use serde::Deserialize;

use crate::stats::{AgentStats, Jsonl, Lines, Prompts, Tally, Text, leading_type};

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
}

impl Totals {
    fn count(&mut self, usage: &Usage) {
        self.output += usage.output;
        if let Some(total) = usage.cost.as_ref().and_then(|c| c.total) {
            self.cost += total;
            self.costed = true;
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
        match entry.kind.as_deref() {
            Some("message") => {
                let message = entry.message.unwrap_or_default();
                match message.role.as_deref() {
                    Some("assistant") => {
                        if let Some(usage) = message.usage {
                            self.count(&usage);
                            let context =
                                usage.input + usage.output + usage.cache_read + usage.cache_write;
                            if context > 0 {
                                self.context = Some(context);
                            }
                        }
                    }
                    Some("user") => {
                        if let Some(Text(Some(text))) = message.content
                            && !text.trim().is_empty()
                        {
                            self.prompts.push(text);
                        }
                    }
                    _ => {}
                }
            }
            // Model calls outside the conversation (cache warming, say).
            Some("usage") => {
                if let Some(usage) = entry.usage {
                    self.count(&usage);
                }
            }
            Some("compaction") => self.context = None,
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
    message: Option<Message>,
    usage: Option<Usage>,
}

#[derive(Default, Deserialize)]
struct Message {
    role: Option<String>,
    content: Option<Text>,
    usage: Option<Usage>,
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
        Some(b"usage" | b"compaction") => true,
        Some(b"message") => {
            let head = &line[..line.len().min(400)];
            match memchr::memmem::find(head, br#""role":""#) {
                Some(i) => {
                    let role = &head[i + br#""role":""#.len()..];
                    role.starts_with(b"user\"") || role.starts_with(b"assistant\"")
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
