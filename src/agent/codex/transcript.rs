//! Codex's rollout: the JSONL file where it keeps a conversation, one entry per
//! line, each with a `type` and a `payload` that names its own kind. An
//! internal format that may change (it's already kept two ways, as `legacy`
//! and `paginated` history), so whatever can't be read is simply left out.

use std::collections::HashMap;

use serde::Deserialize;

use super::tokens::uuid_ms;
use crate::stats::{AgentStats, Jsonl, Lines, Prompts, Tally, Text};
use crate::util::iso_ms;

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
    /// A compaction was just written: the count codex writes after it is the
    /// compaction's own call's, of the context before it, so the context
    /// stays unknown until another call (its record) or a prompt.
    compacted: bool,
    /// Lines its edits added and removed.
    added: u64,
    removed: u64,
    /// The rollout's own conversation, from its first `session_meta`.
    own: Option<String>,
    met: bool,
    /// A fork's: when it was made (ms).
    forked: Option<i64>,
    /// A fork's copy of the conversation it came from is being read, from
    /// that one's `session_meta` to the fork's first turn of its own: in
    /// context, but neither prompted nor spent here. Once only.
    copying: bool,
    copied: bool,
    /// The conversation's output total as the copy ended (without the
    /// per-call records, its own is what came after).
    before: u64,
}

impl Lines for Totals {
    fn line(&mut self, line: &[u8]) {
        if !wanted(line) {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        if entry.kind.as_deref() == Some("compacted") {
            self.compaction();
            return;
        }
        let Some(payload) = entry.payload else {
            return;
        };
        match entry.kind.as_deref() {
            Some("session_meta") if !self.met => {
                self.met = true;
                self.own = payload.id.clone();
                if payload.forked_from_id.is_some() {
                    let made = payload.timestamp.as_deref().and_then(iso_ms);
                    let written = entry.timestamp.as_deref().and_then(iso_ms);
                    self.forked = Some(made.or(written).unwrap_or(0));
                }
                return;
            }
            // Another's, in a fork: the copy begins.
            Some("session_meta") => {
                if self.forked.is_some() && !self.copied {
                    (self.copying, self.copied) = (true, true);
                }
                return;
            }
            _ => {}
        }
        // A turn: the fork's own first ends the copy (its id is a UUIDv7,
        // the copied ones older than the fork).
        let turn = entry.kind.as_deref() == Some("turn_context")
            || (entry.kind.as_deref() == Some("event_msg")
                && payload.kind.as_deref() == Some("task_started"));
        if turn {
            if self.copying
                && let (Some(made), Some(id)) = (self.forked, payload.turn_id.as_deref())
                && uuid_ms(id).is_some_and(|t| t >= made)
            {
                self.copying = false;
            }
            return;
        }
        // A record names its conversation: in a fork, the fork's own counts.
        let copied = match payload.thread_id.as_deref() {
            Some(thread) if self.forked.is_some() => Some(thread) != self.own.as_deref(),
            _ => self.copying,
        };
        match entry.kind.as_deref() {
            // What the conversation holds: the prompts, and nothing else of
            // it that roster counts.
            Some("response_item") if copied => {
                if payload.kind.as_deref() == Some("compaction") {
                    self.compaction();
                }
            }
            Some("response_item") => match payload.kind.as_deref() {
                Some("message") if payload.role.as_deref() == Some("user") => {
                    if self.typed.message(payload.kinds())
                        && let Some(text) = payload.text()
                    {
                        self.prompt(text);
                    }
                }
                Some("compaction") => self.compaction(),
                _ => {}
            },
            // The tokens of one model call, summed as they come.
            Some("token_usage_record") => {
                if let Some(usage) = payload.usage.as_ref().filter(|_| !copied) {
                    self.output += usage.output_tokens.unwrap_or(0);
                    self.counted = true;
                }
                self.compacted = false;
            }
            Some("event_msg") => match payload.kind.as_deref() {
                Some("token_count") => {
                    let info = payload.info.unwrap_or_default();
                    if let Some(usage) = info.last
                        && usage.total_tokens > 0
                        && !self.compacted
                    {
                        self.context = Some(usage.total_tokens);
                    }
                    if let Some(window) = info.window {
                        self.window = Some(window);
                    }
                    // Without the per-call records, its own total is all there
                    // is: a fork's, beyond what it was as the copy ended.
                    if let Some(total) = info.total.as_ref().and_then(|u| u.output_tokens) {
                        if self.copying {
                            self.before = total;
                        } else if !self.counted {
                            self.output = total.checked_sub(self.before).unwrap_or(total);
                        }
                    }
                }
                Some("context_compacted") => self.compaction(),
                // An edit, as codex records it: since 0.163 an item it
                // completed, before a patch it applied.
                Some("item_completed") if !copied => {
                    if let Some(item) = payload.item.as_deref()
                        && item.kind.as_deref() == Some("FileChange")
                        && item.status.as_deref() == Some("completed")
                    {
                        self.edited(item);
                    }
                }
                Some("patch_apply_end") if payload.success == Some(true) && !copied => {
                    self.edited(&payload)
                }
                Some("user_message") if !copied => {
                    if self.typed.event()
                        && let Some(text) = payload.message.filter(|t| !t.trim().is_empty())
                    {
                        self.prompt(text);
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl Tally for Totals {
    /// Codex says which window it's calling into, so `model` isn't needed.
    /// What a subscription costs it doesn't say at all.
    fn stats(&self, _model: Option<&str>) -> AgentStats {
        AgentStats {
            prompts: Some(self.prompts.count),
            context_used: self.context,
            context_limit: self.window,
            output_tokens: (self.output > 0).then_some(self.output),
            lines_added: Some(self.added),
            lines_removed: Some(self.removed),
            ..AgentStats::default()
        }
    }

    fn prompts(&self) -> &Prompts {
        &self.prompts
    }
}

impl Totals {
    fn compaction(&mut self) {
        self.context = None;
        self.compacted = true;
    }

    /// The lines an edit changed: a file's added or deleted whole, an updated
    /// one's by its diff.
    fn edited(&mut self, edit: &Payload) {
        for change in edit.changes.iter().flat_map(HashMap::values) {
            let lines =
                |text: &Option<String>| text.as_deref().map_or(0, |t| t.lines().count() as u64);
            match change.kind.as_deref() {
                Some("add") => self.added += lines(&change.content),
                Some("delete") => self.removed += lines(&change.content),
                // Its hunks: codex writes no file header (`--- a/x`), and
                // one would come before the first, so a line `--- x` in a
                // hunk is a line removed.
                Some("update") => {
                    let diff = change.unified_diff.as_deref().unwrap_or("");
                    for line in diff.lines().skip_while(|l| !l.starts_with("@@")) {
                        if line.starts_with('+') {
                            self.added += 1;
                        } else if line.starts_with('-') {
                            self.removed += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// A prompt starts a turn: its calls count again.
    fn prompt(&mut self, text: String) {
        self.prompts.push(text);
        self.compacted = false;
    }
}

/// What roster reads of an entry; the rest of it is skipped unread.
#[derive(Deserialize)]
struct Entry {
    timestamp: Option<String>,
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
    content: Option<Text>,
    usage: Option<Usage>,
    info: Option<Info>,
    /// A `user_message` event's text.
    message: Option<String>,
    /// An edit's: whether it was applied (a patch's, or an item's status),
    /// and what it changed, by file.
    success: Option<bool>,
    status: Option<String>,
    changes: Option<HashMap<String, Change>>,
    /// The item an `item_completed` completed.
    item: Option<Box<Payload>>,
    #[serde(rename = "internal_chat_message_metadata_passthrough")]
    metadata: Option<Metadata>,
    /// A `session_meta`'s: its conversation, the one it was forked from,
    /// and when.
    id: Option<String>,
    forked_from_id: Option<String>,
    timestamp: Option<String>,
    /// A turn's id; a record's conversation.
    turn_id: Option<String>,
    thread_id: Option<String>,
}

/// What an edit did to a file: added or deleted it, with its content, or
/// updated it, with a diff.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Change {
    #[serde(rename = "type")]
    kind: Option<String>,
    content: Option<String>,
    unified_diff: Option<String>,
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

    /// The text of a message (`Text`).
    fn text(&self) -> Option<String> {
        self.content.as_ref()?.0.clone()
    }
}

/// The effort codex last ran a turn with, as the rollout at `path` says at
/// its end: each turn starts with a `turn_context` that names it.
pub(super) fn last_effort(path: &std::path::Path) -> Option<String> {
    let entry = last_entry(path, br#""type":"turn_context""#, |_| true)?;
    entry["payload"]["effort"].as_str().map(String::from)
}

/// What the account had used of its limits as the rollout at `path` last
/// said, as of when it said so (the entry's `timestamp`, which codex gives
/// every line: the file changes with whatever codex writes after): codex
/// writes them with every response's `token_count`.
pub(super) fn last_limits(path: &std::path::Path) -> Option<crate::usage::Usage> {
    let limits = |entry: &serde_json::Value| {
        let at = crate::util::iso_ms(entry["timestamp"].as_str()?)?;
        super::limits::limits(&entry["payload"]["rate_limits"], at)
    };
    limits(&last_entry(path, br#""rate_limits":{"#, |e| {
        limits(e).is_some()
    })?)
}

/// The rollout at `path`'s last entry holding `needle` that `wanted` takes,
/// looked for in its last MB only, each such line alone parsed: a turn that
/// wrote more since leaves it unknown.
fn last_entry(
    path: &std::path::Path,
    needle: &[u8],
    wanted: impl Fn(&serde_json::Value) -> bool,
) -> Option<serde_json::Value> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 1024 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    memchr::memmem::rfind_iter(&tail, needle).find_map(|at| {
        let start = memchr::memrchr(b'\n', &tail[..at]).map_or(0, |i| i + 1);
        let end = memchr::memchr(b'\n', &tail[at..]).map_or(tail.len(), |i| at + i);
        serde_json::from_slice(&tail[start..end])
            .ok()
            .filter(|entry| wanted(entry))
    })
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
        br#""session_meta""#,
        br#""turn_context""#,
        br#""task_started""#,
        br#""response_item""#,
        br#""user_message""#,
        br#""token_usage_record""#,
        br#""token_count""#,
        br#""compaction""#,
        br#""FileChange""#,
        br#""patch_apply_end""#,
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
    use serde_json::{Value, json};
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
    fn a_fork_counts_what_it_spent_itself() {
        // A UUIDv7 made at `ms`, as codex names turns.
        let turn_id = |ms: i64| {
            let hex = format!("{ms:012x}");
            format!("{}-{}-7000-8000-000000000000", &hex[..8], &hex[8..])
        };
        let made = iso_ms("2026-06-05T17:08:53.197Z").unwrap();
        let task = |ms: i64| {
            json!({"type": "event_msg", "payload": {"type": "task_started", "turn_id": turn_id(ms)}})
                .to_string()
        };
        let mut lines = vec![
            json!({"timestamp": "2026-06-05T17:08:53.240Z", "type": "session_meta", "payload": {"id": "fork",
                "forked_from_id": "parent", "timestamp": "2026-06-05T17:08:53.197Z"}})
            .to_string(),
            // The copy: the parent's meta, a turn of its, and what it spent.
            json!({"type": "session_meta", "payload": {"id": "parent"}}).to_string(),
            task(made - 60_000),
        ];
        lines.extend(turn("in the parent", 100, 1000, 400_000));
        // More of the copy: a prompt as codex wrote it before 0.157, an
        // edit, and a record naming the parent.
        lines.push(user("unmarked, in the parent", None));
        lines.push(user_event("unmarked, in the parent"));
        lines.push(
            json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "FileChange",
                "status": "completed", "changes": {"a.rs": {"type": "add", "content": "x\ny\n"}}}}})
            .to_string(),
        );
        lines.push(
            json!({"type": "token_usage_record", "payload": {"thread_id": "parent",
                "usage": {"input_tokens": 1, "output_tokens": 30, "total_tokens": 31}}})
            .to_string(),
        );
        // Its own.
        lines.push(task(made + 50));
        lines.extend(turn("in the fork", 7, 1200, 400_000));
        let (stats, prompts) = totals(&lines);
        assert_eq!(
            (stats.prompts, stats.output_tokens, stats.context_used),
            (Some(1), Some(7), Some(1200))
        );
        assert_eq!((stats.lines_added, stats.lines_removed), (Some(0), Some(0)));
        assert_eq!(prompts, ["in the fork"]);
        // Without the per-call records (before 0.163): its own is what its
        // total grew by after the copy.
        let unrecorded: Vec<String> = lines
            .iter()
            .filter(|l| !l.contains("token_usage_record"))
            .map(|l| l.replace("\"output_tokens\":7,", "\"output_tokens\":107,"))
            .collect();
        assert!(
            unrecorded
                .iter()
                .any(|l| l.contains("\"output_tokens\":107,"))
        );
        let (stats, _) = totals(&unrecorded);
        assert_eq!(stats.output_tokens, Some(7));
    }

    /// A UUIDv7 made at `ms`, as codex names turns.
    fn uuid7(ms: i64) -> String {
        let hex = format!("{ms:012x}");
        format!("{}-{}-7000-8000-000000000000", &hex[..8], &hex[8..])
    }

    /// A turn starting at `ms`, as codex writes it.
    fn task_started(ms: i64) -> String {
        json!({"type": "event_msg", "payload": {"type": "task_started", "turn_id": uuid7(ms)}})
            .to_string()
    }

    /// A fork's first line, written at `written`, its own time `made` if
    /// it says it.
    fn fork_meta(written: &str, made: Option<&str>) -> String {
        let mut payload = json!({"id": "fork", "forked_from_id": "parent", "cwd": "/w"});
        if let Some(made) = made {
            payload["timestamp"] = json!(made);
        }
        json!({"timestamp": written, "type": "session_meta", "payload": payload}).to_string()
    }

    /// A patch applied adding `file`, as codex wrote it before 0.157.
    fn patch_added(file: &str, content: &str) -> String {
        json!({"type": "event_msg", "payload": {"type": "patch_apply_end", "success": true,
            "changes": {file: {"type": "add", "content": content}}}})
        .to_string()
    }

    #[test]
    fn in_a_fork_the_parents_patches_and_records_count_nothing() {
        let made = iso_ms("2026-06-05T17:08:53.197Z").unwrap();
        let mut lines = vec![
            fork_meta("2026-06-05T17:08:53.240Z", Some("2026-06-05T17:08:53.197Z")),
            json!({"type": "session_meta", "payload": {"id": "parent"}}).to_string(),
            task_started(made - 60_000),
        ];
        lines.extend(turn("in the parent", 100, 1000, 400_000));
        // An edit in the copy.
        lines.push(patch_added("/w/a.rs", "x\ny\n"));
        // Its own turn, and edit.
        lines.push(task_started(made + 50));
        lines.extend(turn("in the fork", 7, 1200, 400_000));
        lines.push(patch_added("/w/b.rs", "z\n"));
        // Codex counting the parent's call it shows the fork, after the
        // fork's own turn began: the record names the parent.
        lines.push(
            json!({"type": "token_usage_record", "payload": {"thread_id": "parent",
                "response_id": "resp_p", "usage": {"input_tokens": 1, "output_tokens": 30, "total_tokens": 31}}})
            .to_string(),
        );
        let (stats, prompts) = totals(&lines);
        assert_eq!(
            (stats.output_tokens, stats.lines_added, stats.lines_removed),
            (Some(7), Some(1), Some(0))
        );
        assert_eq!(prompts, ["in the fork"]);
    }

    #[test]
    fn a_fork_was_made_when_its_meta_says_else_when_written() {
        let made = iso_ms("2026-06-05T17:08:53.197Z").unwrap();
        // Its own first turn began after it was made, before its first line
        // was written.
        let fork = |meta: String| {
            let mut lines = vec![
                meta,
                json!({"type": "session_meta", "payload": {"id": "parent"}}).to_string(),
                task_started(made - 60_000),
            ];
            lines.extend(turn("in the parent", 100, 1000, 400_000));
            lines.push(task_started(made + 20));
            lines.extend(turn("in the fork", 7, 1200, 400_000));
            totals(&lines)
        };
        let (stats, prompts) = fork(fork_meta(
            "2026-06-05T17:08:53.240Z",
            Some("2026-06-05T17:08:53.197Z"),
        ));
        assert_eq!((stats.prompts, stats.output_tokens), (Some(1), Some(7)));
        assert_eq!(prompts, ["in the fork"]);
        // A meta that doesn't say: when its line was written.
        let (stats, prompts) = fork(fork_meta("2026-06-05T17:08:53.210Z", None));
        assert_eq!((stats.prompts, stats.output_tokens), (Some(1), Some(7)));
        assert_eq!(prompts, ["in the fork"]);
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
    fn the_effort_is_the_last_turns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        assert_eq!(last_effort(&path), None);
        let context = |effort: &str| {
            json!({"timestamp": "t", "type": "turn_context", "payload": {"model": "gpt-6-luna", "effort": effort}})
                .to_string()
        };
        let mut lines = turn("first", 5, 115, 400_000);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert_eq!(last_effort(&path), None);
        lines.insert(0, context("medium"));
        lines.push(context("xhigh"));
        lines.extend(turn("second", 7, 230, 400_000));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert_eq!(last_effort(&path).as_deref(), Some("xhigh"));
    }

    #[test]
    fn the_limits_are_the_last_the_account_was_said_to_have() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        assert_eq!(last_limits(&path), None);
        let count = |id: &str, used: f64, at: &str| {
            json!({"timestamp": at, "type": "event_msg", "payload": {"type": "token_count", "info": null,
                "rate_limits": {"limit_id": id,
                    "primary": {"used_percent": used, "window_minutes": 300, "resets_at": 100},
                    "secondary": {"used_percent": 3.0, "window_minutes": 10_080, "resets_at": 200},
                    "plan_type": "plus"}}})
            .to_string()
        };
        let mut lines = turn("first", 5, 115, 400_000);
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert_eq!(last_limits(&path), None);
        lines.push(count("codex", 11.0, "2026-10-25T09:34:21.383Z"));
        lines.push(count("codex", 17.0, "2026-10-25T09:35:00.000Z"));
        // Another kind of limit's, after them, isn't the account's.
        lines.push(count("premium", 90.0, "2026-10-25T09:36:00.000Z"));
        lines.extend(turn("second", 7, 230, 400_000));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let usage = last_limits(&path).unwrap();
        assert_eq!(usage.windows[0].used, 17.0);
        assert_eq!(usage.windows[0].resets_at, Some(100_000));
        // As of when it was said, whatever codex wrote since.
        let said = crate::util::iso_ms("2026-10-25T09:35:00.000Z");
        assert_eq!(Some(usage.at), said);
        lines.push(
            json!({"timestamp": "2026-10-25T10:00:00.000Z", "type": "event_msg", "payload": {"type": "task_started"}})
                .to_string(),
        );
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert_eq!(Some(last_limits(&path).unwrap().at), said);
        // One with no time isn't taken: when it held isn't known.
        let mut untimed: Value = serde_json::from_str(&count("codex", 50.0, "")).unwrap();
        untimed.as_object_mut().unwrap().remove("timestamp");
        lines.push(untimed.to_string());
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        assert_eq!(last_limits(&path).unwrap().windows[0].used, 17.0);
    }

    #[test]
    fn counts_the_lines_its_edits_changed() {
        let file_change = |status: &str, changes: Value| {
            json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {
                "type": "FileChange", "id": "exec-1", "changes": changes, "status": status}}})
            .to_string()
        };
        let mut lines = turn("make notes", 5, 115, 400_000);
        // As 0.164 records them: a file added, then updated.
        lines.push(file_change(
            "completed",
            json!({"/w/notes.txt": {"type": "add", "content": "one\ntwo\nthree\n"}}),
        ));
        lines.push(file_change(
            "completed",
            json!({"/w/notes.txt": {"type": "update", "move_path": null,
                "unified_diff": "@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n"}}),
        ));
        // Lines that look like a diff's file header are lines too: a comment
        // in SQL removed, an increment added.
        lines.push(file_change(
            "completed",
            json!({"/w/q.sql": {"type": "update", "move_path": null,
                "unified_diff": "@@ -1,2 +1,2 @@\n--- old comment\n-x\n+++i;\n+y\n"}}),
        ));
        // A header, which codex doesn't write, isn't.
        lines.push(file_change(
            "completed",
            json!({"/w/z.txt": {"type": "update", "move_path": null,
                "unified_diff": "--- a/w/z.txt\n+++ b/w/z.txt\n@@ -1 +1 @@\n-a\n+b\n"}}),
        ));
        // One that failed changed nothing.
        lines.push(file_change(
            "failed",
            json!({"/w/x.txt": {"type": "add", "content": "a\nb\n"}}),
        ));
        // As before 0.157: a patch applied, deleting a file.
        lines.push(
            json!({"type": "event_msg", "payload": {"type": "patch_apply_end", "success": true,
                "changes": {"/w/old.txt": {"type": "delete", "content": "gone\nalso gone\n"}}}})
            .to_string(),
        );
        let (stats, _) = totals(&lines);
        assert_eq!((stats.lines_added, stats.lines_removed), (Some(7), Some(6)));
        // Nothing edited: nothing to show.
        let (stats, _) = totals(&turn("look", 5, 115, 400_000));
        assert_eq!((stats.lines_added, stats.lines_removed), (Some(0), Some(0)));
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
            // As 0.164 writes it: the compaction's own call is counted after
            // it, with the context it compacted.
            lines.push(
                json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
                    "total_token_usage": {"output_tokens": 9, "total_tokens": 300},
                    "last_token_usage": {"total_tokens": 300},
                    "model_context_window": 400_000}}})
                .to_string(),
            );
            let (stats, _) = totals(&lines);
            assert_eq!(stats.context_used, None, "{compacted}");
            assert_eq!(stats.output_tokens, Some(5));
            // The next call says what's in context again.
            lines.extend(turn("go on", 7, 50, 400_000));
            assert_eq!(totals(&lines).0.context_used, Some(50));
        }
    }
}
