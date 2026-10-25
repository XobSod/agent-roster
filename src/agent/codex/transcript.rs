//! Codex's rollout: the JSONL file where it keeps a conversation, one entry per
//! line, each with a `type` and a `payload` that names its own kind. An
//! internal format that may change (it's already kept two ways, as `legacy`
//! and `paginated` history), so whatever can't be read is simply left out.

use serde::Deserialize;

use crate::stats::{AgentStats, Jsonl, Prompts, Tally};

/// Reads a codex rollout as it grows.
pub type CodexTranscript = Jsonl<Totals>;

/// A rollout's running totals.
#[derive(Debug, Default)]
pub struct Totals {
    prompts: Prompts,
    typed: Typed,
    /// Output tokens, counted once per model call.
    output: u64,
    /// What the last call sent: the tokens in context. Unknown after a
    /// compaction until the next one.
    context: Option<u64>,
    /// The window of the model it last called, as codex reports it.
    window: Option<u64>,
    /// Output tokens summed from the per-call records, if there were any.
    counted: bool,
}

impl Tally for Totals {
    fn line(&mut self, line: &[u8]) {
        if !wanted(line) {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        if entry.kind.as_deref() == Some("compacted") {
            self.context = None;
            return;
        }
        let Some(payload) = entry.payload else {
            return;
        };
        match entry.kind.as_deref() {
            // What the conversation holds: the prompts, and nothing else of
            // it that roster counts.
            Some("response_item") => match payload.kind.as_deref() {
                Some("message") if payload.role.as_deref() == Some("user") => {
                    if self.typed.message(payload.kinds())
                        && let Some(text) = payload.text()
                    {
                        self.prompts.push(text);
                    }
                }
                Some("compaction") => self.context = None,
                _ => {}
            },
            // The tokens of one model call, summed as they come.
            Some("token_usage_record") => {
                if let Some(usage) = payload.usage.as_ref() {
                    self.output += usage.output_tokens.unwrap_or(0);
                    self.counted = true;
                }
            }
            Some("event_msg") => match payload.kind.as_deref() {
                Some("token_count") => {
                    let info = payload.info.unwrap_or_default();
                    if let Some(usage) = info.last
                        && usage.total_tokens > 0
                    {
                        self.context = Some(usage.total_tokens);
                    }
                    if let Some(window) = info.window {
                        self.window = Some(window);
                    }
                    // Without the per-call records, its own total is all there is.
                    if !self.counted {
                        self.output = info
                            .total
                            .as_ref()
                            .and_then(|u| u.output_tokens)
                            .unwrap_or(self.output);
                    }
                }
                Some("context_compacted") => self.context = None,
                Some("user_message") => {
                    if self.typed.event()
                        && let Some(text) = payload.message.filter(|t| !t.trim().is_empty())
                    {
                        self.prompts.push(text);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// Codex says which window it's calling into, so `model` isn't needed.
    /// What a subscription costs it doesn't say at all.
    fn stats(&self, _model: Option<&str>) -> AgentStats {
        AgentStats {
            prompts: Some(self.prompts.count),
            context_used: self.context,
            context_limit: self.window,
            output_tokens: (self.output > 0).then_some(self.output),
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
    payload: Option<Payload>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    role: Option<String>,
    content: Option<Content>,
    usage: Option<Usage>,
    info: Option<Info>,
    /// A `user_message` event's text.
    message: Option<String>,
    #[serde(rename = "internal_chat_message_metadata_passthrough")]
    metadata: Option<Metadata>,
}

/// What codex notes of a message beside it: the kind of each of its parts.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Metadata {
    content_item_kinds: Option<Vec<String>>,
}

/// Tokens, as codex counts them: `output_tokens` of a call, `total_tokens` of
/// a request (what was in context for it).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Usage {
    #[serde(rename = "outputTokens", alias = "output_tokens")]
    output_tokens: Option<u64>,
    #[serde(alias = "total_tokens")]
    total_tokens: u64,
}

/// A `token_count`'s counts: the session's, the last call's, and the window.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Info {
    #[serde(rename = "totalTokenUsage", alias = "total_token_usage")]
    total: Option<Usage>,
    #[serde(rename = "lastTokenUsage", alias = "last_token_usage")]
    last: Option<Usage>,
    #[serde(rename = "modelContextWindow", alias = "model_context_window")]
    window: Option<u64>,
}

impl Payload {
    fn kinds(&self) -> Option<&[String]> {
        self.metadata.as_ref()?.content_item_kinds.as_deref()
    }

    /// The text of a message: one string, or the text blocks of a list (the
    /// images, files and tool outputs in it are skipped).
    fn text(&self) -> Option<String> {
        match self.content.as_ref()? {
            Content::Text(text) => Some(text.clone()),
            Content::Blocks(blocks) => {
                let parts: Vec<&str> = blocks
                    .iter()
                    .filter(|b| {
                        b.kind
                            .as_deref()
                            .is_none_or(|k| matches!(k, "input_text" | "text"))
                    })
                    .filter_map(|b| b.text.as_deref())
                    .collect();
                (!parts.is_empty()).then(|| parts.join("\n"))
            }
        }
    }
}

/// A message's content, either way codex writes it.
#[derive(Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Block {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

/// Which of a rollout's user messages are prompts you typed, as codex says
/// itself rather than by their text: codex gives the model context as user
/// messages too. Since 0.157 it marks each message with the kinds of its parts
/// (`user.text` for yours; `environments.environment_context`,
/// `agents_md.instructions` and the like for its own). Before, it marked none,
/// and wrote what you typed again as a `user_message` event after it, which
/// 0.124 did beside marked ones too. So a marked message is a prompt by its
/// kinds, and an event only after an unmarked one: nothing counts twice.
#[derive(Debug)]
pub(super) struct Typed {
    after_unmarked: bool,
}

impl Default for Typed {
    /// As if after an unmarked message: an event read first (the end of a
    /// rollout read without its middle) is the prompt of one not read.
    fn default() -> Self {
        Typed {
            after_unmarked: true,
        }
    }
}

impl Typed {
    /// A user message, with the kinds codex marked it with: whether it's a
    /// prompt.
    pub(super) fn message(&mut self, kinds: Option<&[String]>) -> bool {
        self.after_unmarked = kinds.is_none();
        kinds.is_some_and(|kinds| kinds.iter().any(|k| k == "user.text"))
    }

    /// A `user_message` event: whether it's a prompt not counted already.
    pub(super) fn event(&mut self) -> bool {
        std::mem::take(&mut self.after_unmarked)
    }
}

/// The lines worth parsing: codex writes a line per thing that happens, and
/// most of them say nothing roster counts. (Its `type` comes after its
/// timestamp, so `stats::leading_type` can't tell them apart.)
fn wanted(line: &[u8]) -> bool {
    const MARKS: &[&[u8]] = &[
        br#""response_item""#,
        br#""user_message""#,
        br#""token_usage_record""#,
        br#""token_count""#,
        br#""compaction""#,
        br#""compacted""#,
        br#""context_compacted""#,
    ];
    MARKS
        .iter()
        .any(|mark| memchr::memmem::find(line, mark).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Transcript;
    use serde_json::json;
    use std::fs::OpenOptions;
    use std::io::Write;

    /// A user message as codex writes it, marked with its kinds as since 0.157
    /// (None: unmarked, as before).
    fn user(text: &str, kinds: Option<&[&str]>) -> String {
        let mut payload = json!({"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": text}, {"type": "image", "image_url": "…"}]});
        if let Some(kinds) = kinds {
            payload["internal_chat_message_metadata_passthrough"] =
                json!({"turn_id": "t1", "content_item_kinds": kinds});
        }
        json!({"timestamp": "t", "type": "response_item", "payload": payload}).to_string()
    }

    /// What codex wrote, before 0.157, of what you typed.
    fn user_event(text: &str) -> String {
        json!({"type": "event_msg", "payload": {"type": "user_message", "message": text, "images": []}})
            .to_string()
    }

    /// A turn as codex writes it: your prompt, what it cost, and where it is.
    fn turn(text: &str, output: u64, total: u64, window: u64) -> Vec<String> {
        vec![
            user(text, Some(&["user.text"])),
            json!({"timestamp": "t", "type": "response_item", "payload": {"type": "message",
                "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}})
            .to_string(),
            json!({"type": "token_usage_record", "payload": {"usage": {"input_tokens": 10,
                "output_tokens": output, "total_tokens": total}}})
            .to_string(),
            json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                "total_token_usage": {"output_tokens": output, "total_tokens": total},
                "last_token_usage": {"total_tokens": total},
                "model_context_window": window},
                "rate_limits": {"credits": {"has_credits": false}}}})
            .to_string(),
        ]
    }

    /// What a rollout adds up to, and the prompts it remembers: read whole,
    /// from a file that goes with the temporary directory.
    fn totals(lines: &[String]) -> (AgentStats, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout-2026-10-22T10-00-00-01a1.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut transcript = CodexTranscript::new(&path);
        transcript.update().unwrap();
        (transcript.stats(None), transcript.recent_prompts())
    }

    #[test]
    fn counts_prompts_tokens_and_the_window() {
        let lines: Vec<String> = turn("fix it", 5, 115, 400_000)
            .into_iter()
            .chain(turn("and the tests", 7, 230, 400_000))
            .collect();
        let (stats, prompts) = totals(&lines);
        assert_eq!(stats.prompts, Some(2));
        assert_eq!(stats.output_tokens, Some(12));
        assert_eq!(stats.context_used, Some(230));
        assert_eq!(stats.context_limit, Some(400_000));
        // What a subscription costs it, codex doesn't say.
        assert_eq!(stats.cost_usd, None);
        assert_eq!(prompts, ["fix it", "and the tests"]);
    }

    #[test]
    fn counts_a_prompt_once_and_skips_what_codex_injects() {
        let mut lines = vec![
            user(
                "<environment_context>…</environment_context>",
                Some(&["environments.environment_context"]),
            ),
            user(
                "# AGENTS.md instructions for /work/app\n\n<INSTRUCTIONS>\nbe brief\n</INSTRUCTIONS>",
                Some(&["agents_md.instructions", "environments.environment_context"]),
            ),
        ];
        lines.extend(turn("fix it", 5, 115, 400_000));
        // Yours however it begins: codex says it's yours.
        lines.push(user("<task>Fix the bug</task>", Some(&["user.text"])));
        // Codex 0.124 wrote the event beside a marked message: still one.
        lines.push(user("and the docs", Some(&["user.text"])));
        lines.push(user_event("and the docs"));
        // A developer message, a tool call, and a line that isn't JSON.
        lines.push(
            json!({"type": "response_item", "payload": {"type": "message", "role": "developer",
                "content": [{"type": "input_text", "text": "instructions"}]}})
            .to_string(),
        );
        lines.push(json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "arguments": "{}"}}).to_string());
        lines.push("{not json".into());
        let (stats, prompts) = totals(&lines);
        assert_eq!(stats.prompts, Some(3));
        assert_eq!(
            prompts,
            ["fix it", "<task>Fix the bug</task>", "and the docs"]
        );
    }

    #[test]
    fn reads_the_prompts_of_an_older_codex_from_its_events() {
        // Before 0.157 nothing was marked; what you typed came again as an
        // event. A conversation resumed by a newer codex goes on marked.
        let lines = [
            user("<environment_context>…</environment_context>", None),
            user("<task>Fix the bug</task>", None),
            user_event("<task>Fix the bug</task>"),
            user("<turn_aborted>", None),
            user("go on", Some(&["user.text"])),
        ];
        let (stats, prompts) = totals(&lines);
        assert_eq!(stats.prompts, Some(2));
        assert_eq!(prompts, ["<task>Fix the bug</task>", "go on"]);
    }

    #[test]
    fn reads_what_was_added_since_the_last_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut t = CodexTranscript::new(&path);
        t.update().unwrap();
        assert_eq!(t.stats(None).prompts, Some(0));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(turn("first", 5, 115, 400_000).join("\n").as_bytes())
            .unwrap();
        file.write_all(b"\n").unwrap();
        t.update().unwrap();
        assert_eq!(t.stats(None).context_used, Some(115));
        // Half a line: it waits for the rest, and counts nothing twice.
        file.write_all(b"{\"type\": \"token_usage_record\", \"payload\": {\"usage\": {\"output")
            .unwrap();
        t.update().unwrap();
        assert_eq!(t.stats(None).output_tokens, Some(5));
        file.write_all(b"_tokens\": 5, \"total_tokens\": 115}}}\n")
            .unwrap();
        file.write_all(turn("second", 7, 230, 400_000).join("\n").as_bytes())
            .unwrap();
        file.write_all(b"\n").unwrap();
        t.update().unwrap();
        let stats = t.stats(None);
        assert_eq!(stats.prompts, Some(2));
        assert_eq!(stats.output_tokens, Some(17));
        assert_eq!(stats.context_used, Some(230));
        assert_eq!(t.recent_prompts(), ["first", "second"]);
    }

    #[test]
    fn a_compaction_leaves_the_context_unknown_until_the_next_call() {
        // As codex writes it now (its paged history keeps no
        // `context_compacted` event), and as it did.
        for compacted in [
            json!({"timestamp": "t", "type": "compacted", "payload": {"message": "summary"}}),
            json!({"type": "event_msg", "payload": {"type": "context_compacted"}}),
        ] {
            let mut lines = turn("fix it", 5, 115, 400_000);
            lines.push(compacted.to_string());
            let (stats, _) = totals(&lines);
            assert_eq!(stats.context_used, None, "{compacted}");
            assert_eq!(stats.output_tokens, Some(5));
            // The next call says what's in context again.
            lines.extend(turn("go on", 7, 50, 400_000));
            assert_eq!(totals(&lines).0.context_used, Some(50));
        }
    }
}
