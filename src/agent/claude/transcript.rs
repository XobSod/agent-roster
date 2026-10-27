//! Claude's transcript: the JSONL file where Claude Code keeps a conversation.
//! An internal format that may change, so whatever can't be read is simply
//! left out.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::value::RawValue;

use super::tokens::{Replies, subagent_files};
use crate::stats::{AgentStats, Jsonl, Lines, Prompts, Tally, Text, Transcript, leading_type};
use crate::tokens::Calls;

/// Reads a Claude conversation as it grows: its transcript, and its
/// subagents' beside it (`<id>/subagents/`), whose replies aren't in it,
/// for their output (Claude's own cost, in the transcript, has them). Those
/// only once its statistics are asked for: for its prompts alone (a
/// heading's preview, a title), they'd be read for nothing.
pub struct ClaudeTranscript {
    main: Jsonl<Totals>,
    subagents: HashMap<PathBuf, Jsonl<Replies>>,
    counted: std::cell::Cell<bool>,
    /// A copy's: the conversation it was copied from, and when, until that
    /// conversation is read (at the first update).
    fork: Option<(String, i64)>,
}

impl ClaudeTranscript {
    pub fn new(path: &Path) -> ClaudeTranscript {
        ClaudeTranscript {
            main: Jsonl::new(path),
            subagents: HashMap::new(),
            counted: std::cell::Cell::new(false),
            fork: None,
        }
    }

    /// What a copy roster made was copied from: the conversation `from`, in
    /// its file beside this one, read as it was then; or, where it can't be
    /// read, when it was copied (at `at`).
    fn copy_of(&self, from: &str, at: i64) -> Origin {
        let parent = (!from.is_empty() && !from.contains('/')).then(|| {
            let mut parent =
                Jsonl::<Parent>::new(&self.main.path().with_file_name(format!("{from}.jsonl")));
            parent.read().map(|()| std::mem::take(parent.tally_mut()))
        });
        match parent {
            Some(Ok(parent)) => Origin::Of(std::sync::Arc::new(parent)),
            _ => Origin::Before(at),
        }
    }

    fn read_subagents(&mut self) {
        let dir = self.main.path().with_extension("").join("subagents");
        for file in subagent_files(&dir) {
            let reader = self
                .subagents
                .entry(file)
                .or_insert_with_key(|path| Jsonl::new(path));
            // One that can't be read now is read next time.
            let _ = reader.read();
        }
    }
}

impl Transcript for ClaudeTranscript {
    fn path(&self) -> &Path {
        self.main.path()
    }

    fn update(&mut self) -> std::io::Result<()> {
        // Once Claude has written the copy, which it does with its first
        // message: what it copied is in the conversation's file by then,
        // whatever that conversation gained meanwhile.
        if self.fork.is_some() && self.main.path().exists() {
            let (from, at) = self.fork.take().unwrap();
            self.main.tally_mut().copy = Some(self.copy_of(&from, at));
        }
        self.main.update()?;
        if self.counted.get() {
            self.read_subagents();
        }
        Ok(())
    }

    fn stats(&self, model: Option<&str>) -> AgentStats {
        // From the next update on, its subagents' are read too.
        self.counted.set(true);
        let mut stats = self.main.stats(model);
        // Each reply once, whichever of their files it's in; one the
        // transcript has too is the session's own (a forked subagent's file
        // begins with a copy of the reply it was forked from).
        let own = &self.main.tally().outputs;
        let mut replies: HashMap<&str, u64> = HashMap::new();
        for call in self.subagents.values().flat_map(|r| r.tally().calls()) {
            if (call.id.strip_prefix("claude:")).is_some_and(|id| own.contains_key(id)) {
                continue;
            }
            let output = replies.entry(call.id.as_str()).or_default();
            *output = (*output).max(call.counts.output);
        }
        let theirs: u64 = replies.values().sum();
        if theirs > 0 {
            stats.output_tokens = Some(stats.output_tokens.unwrap_or(0) + theirs);
        }
        stats
    }

    fn recent_prompts(&self) -> Vec<String> {
        self.main.recent_prompts()
    }

    /// A copy roster made (`--resume … --fork-session`) is written with the
    /// conversation it came from, each line unmarked (as dated then, with its
    /// uuid), and Claude carries that conversation's cost over.
    fn forked(&mut self, from: &str, at: i64) {
        self.fork = Some((from.to_string(), at));
    }
}

/// What a copy roster made was copied from.
#[derive(Debug)]
enum Origin {
    /// The conversation, as its file has it.
    Of(std::sync::Arc<Parent>),
    /// Only when it was copied (ms), the conversation's file gone: lines
    /// dated before are the copy, and the cost carried over is unknown.
    Before(i64),
}

/// What a copy's lines are told apart by, and its cost measured from: the
/// conversation it was copied from.
#[derive(Debug, Default)]
struct Parent {
    /// Each of its messages' place among them, by its uuid, which a copy's
    /// keeps.
    at: HashMap<String, usize>,
    messages: usize,
    /// Its cost-states, each after how many of its messages.
    costs: Vec<(usize, Cost)>,
}

impl Lines for Parent {
    fn line(&mut self, line: &[u8]) {
        if !wanted(line) {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        match entry.kind.as_deref() {
            Some("user" | "assistant") => {
                if let Some(uuid) = entry.uuid {
                    self.at.entry(uuid.into_owned()).or_insert(self.messages);
                    self.messages += 1;
                }
            }
            Some("cost-state") => {
                if let Some(cost) = entry.cost() {
                    self.costs.push((self.messages, cost));
                }
            }
            _ => {}
        }
    }
}

impl Parent {
    /// Its cost when Claude took it to copy, its message at `last` the last
    /// copied: the last cost-state written before its next. Claude writes one
    /// as it exits, after the messages it counts.
    fn cost_until(&self, last: Option<usize>) -> Cost {
        let next = last.map_or(0, |i| i + 1);
        (self.costs.iter().rev())
            .find(|(after, _)| *after <= next)
            .map_or_else(Cost::default, |(_, cost)| *cost)
    }
}

/// A cost-state's: what the conversation cost, and its lines added and
/// removed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Cost {
    usd: f64,
    added: u64,
    removed: u64,
}

impl Cost {
    /// What it adds to `before`.
    fn since(self, before: Cost) -> Cost {
        Cost {
            usd: (self.usd - before.usd).max(0.0),
            added: self.added.saturating_sub(before.added),
            removed: self.removed.saturating_sub(before.removed),
        }
    }
}

/// A Claude transcript's running totals.
#[derive(Debug, Default)]
pub struct Totals {
    /// Output tokens per reply; Claude writes a reply several times.
    outputs: HashMap<String, u64>,
    context: Option<u64>,
    prompts: Prompts,
    /// The last cost-state.
    cost: Option<Cost>,
    replies_since_cost: bool,
    /// A copy's: what it was copied from.
    copy: Option<Origin>,
    /// The place in that conversation of the last of its messages copied.
    last_copied: Option<usize>,
}

impl Lines for Totals {
    fn line(&mut self, line: &[u8]) {
        if !wanted(line) {
            return;
        }
        let Ok(mut entry) = serde_json::from_slice::<Entry>(line) else {
            return;
        };
        let message = entry.message.take().unwrap_or_default();
        // A subagent's, written into the transcript by an older Claude: its
        // output counts, but its context and prompts are its own.
        let sidechain = entry.is_sidechain == Some(true);
        // A fork's copy of the conversation it came from: in context, but
        // neither prompted nor spent here. /branch marks each line forkedFrom;
        // --fork-session (roster's copies) leaves them with their uuids, as
        // dated then.
        let copied = entry.forked_from.is_some()
            || match &self.copy {
                Some(Origin::Of(parent)) => {
                    let at = entry.uuid.as_deref().and_then(|u| parent.at.get(u));
                    if let Some(&at) = at {
                        self.last_copied = self.last_copied.max(Some(at));
                    }
                    at.is_some()
                }
                Some(Origin::Before(made)) => (entry.timestamp.as_deref())
                    .and_then(crate::util::iso_ms)
                    .is_some_and(|at| at < *made),
                None => false,
            };
        match entry.kind.as_deref() {
            Some("assistant") => {
                let usage = message.usage.unwrap_or_default();
                if let (Some(id), Some(output), false) = (message.id, usage.output_tokens, copied) {
                    self.outputs.insert(id, output);
                }
                if sidechain {
                    return;
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
            Some("user") if entry.is_meta != Some(true) && !sidechain && !copied => {
                let given = entry.prompt_source.is_some();
                if let Some(text) = message.content.and_then(|c| prompt(c, given))
                    && is_prompt(&text, given)
                {
                    self.prompts.push(text);
                }
            }
            Some("cost-state") => {
                if let Some(cost) = entry.cost() {
                    self.cost = Some(cost);
                    self.replies_since_cost = false;
                }
            }
            _ => {}
        }
    }

    fn fresh(&self) -> Totals {
        let copy = match &self.copy {
            Some(Origin::Of(parent)) => Some(Origin::Of(std::sync::Arc::clone(parent))),
            Some(Origin::Before(at)) => Some(Origin::Before(*at)),
            None => None,
        };
        Totals {
            copy,
            ..Totals::default()
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
        // A copy's own: Claude carries over what the conversation it came
        // from had cost.
        let cost = match &self.copy {
            None => self.cost,
            Some(Origin::Of(parent)) => {
                (self.cost).map(|c| c.since(parent.cost_until(self.last_copied)))
            }
            Some(Origin::Before(_)) => None,
        };
        AgentStats {
            prompts: Some(self.prompts.count),
            context_used: self.context,
            context_limit: window,
            output_tokens: (!self.outputs.is_empty()).then(|| self.outputs.values().sum()),
            cost_usd: cost.map(|c| c.usd),
            lines_added: cost.map(|c| c.added),
            lines_removed: cost.map(|c| c.removed),
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
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "forkedFrom")]
    forked_from: Option<IgnoredAny>,
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    uuid: Option<std::borrow::Cow<'a, str>>,
    /// Where a prompt came from, on prompts alone.
    #[serde(rename = "promptSource")]
    prompt_source: Option<IgnoredAny>,
    #[serde(borrow)]
    message: Option<Message<'a>>,
    #[serde(rename = "totalCostUSD")]
    total_cost: Option<f64>,
    #[serde(rename = "totalLinesAdded")]
    lines_added: Option<u64>,
    #[serde(rename = "totalLinesRemoved")]
    lines_removed: Option<u64>,
}

impl Entry<'_> {
    /// A cost-state's.
    fn cost(&self) -> Option<Cost> {
        Some(Cost {
            usd: self.total_cost?,
            added: self.lines_added.unwrap_or(0),
            removed: self.lines_removed.unwrap_or(0),
        })
    }
}

/// A message, its content left unread (often large: replies, tool results)
/// until it's known to be a prompt.
#[derive(Default, Deserialize)]
struct Message<'a> {
    id: Option<String>,
    usage: Option<Usage>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Default, Deserialize)]
struct Usage {
    output_tokens: Option<u64>,
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

/// Whether a user message's text is a prompt the user gave: one Claude marked
/// as given (`promptSource`, since 2.1.283) whatever it begins with, markup
/// too; one unmarked (from an older Claude, or Claude's own: its local
/// commands, shell input and their output) only if it isn't wrapped in tags,
/// as those are. Both of Claude's readers, transcript and conversations, ask.
pub(super) fn is_prompt(text: &str, given: bool) -> bool {
    given || !text.trim_start().starts_with('<')
}

/// A prompt's text, from a user message's content: a string, or the text
/// blocks of a list, which Claude writes for a prompt with an image or a
/// file. A list is read only if it was `given`: Claude marks the prompts it
/// was given (`promptSource`: typed, queued, from the SDK), and its other
/// lists, tool results and its own markers (an interruption), aren't.
fn prompt(content: &RawValue, given: bool) -> Option<String> {
    if content.get().starts_with('"') {
        serde_json::from_str(content.get()).ok()
    } else if given {
        serde_json::from_str::<Text>(content.get()).ok()?.0
    } else {
        None
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
    fn a_prompt_with_an_image_counts_and_claudes_own_lists_dont() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        // As Claude Code 2.1.310 writes them.
        let text = [
            serde_json::json!({"type": "user", "promptSource": "typed", "message": {"role": "user",
                "content": [{"type": "image", "source": {"type": "base64", "data": "…"}},
                            {"type": "text", "text": "What colour is this?"}]}}),
            serde_json::json!({"type": "user", "isMeta": true, "message": {"role": "user",
                "content": [{"type": "text", "text": "[Image: source: /tmp/1.png]"}]}}),
            serde_json::json!({"type": "user", "message": {"role": "user",
                "content": [{"type": "text", "text": "[Request interrupted by user]"}]}}),
            serde_json::json!({"type": "user", "message": {"role": "user",
                "content": [{"type": "tool_result", "content": "output"}]}}),
            serde_json::json!({"type": "user", "promptSource": "typed",
                "message": {"role": "user", "content": "and now?"}}),
            // Markup typed is a prompt; Claude's own, in tags and unmarked, isn't.
            serde_json::json!({"type": "user", "promptSource": "typed",
                "message": {"role": "user", "content": "<task>Fix the bug</task>"}}),
            serde_json::json!({"type": "user",
                "message": {"role": "user", "content": "<bash-input>echo hi</bash-input>"}}),
            serde_json::json!({"type": "user",
                "message": {"role": "user", "content": "<bash-stdout>hi</bash-stdout><bash-stderr></bash-stderr>"}}),
        ]
        .map(line)
        .concat();
        std::fs::write(&path, text).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        assert_eq!(t.stats(None).prompts, Some(3));
        assert_eq!(
            t.recent_prompts(),
            [
                "What colour is this?",
                "and now?",
                "<task>Fix the bug</task>"
            ]
        );
    }

    #[test]
    fn a_subagents_replies_count_for_output_but_not_its_context_or_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c1.jsonl");
        // An older Claude's: the subagent's lines in the transcript.
        let mut text = prompt("fix the tests");
        text += &reply("m1", 500, 1000);
        text += &line(serde_json::json!({"type": "user", "isSidechain": true,
            "message": {"role": "user", "content": "look for the failing test"}}));
        text += &line(serde_json::json!({"type": "assistant", "isSidechain": true,
            "message": {"id": "s1", "usage": {"input_tokens": 3, "output_tokens": 40}}}));
        std::fs::write(&path, &text).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_used),
            (Some(1), Some(540), Some(1012))
        );
        // A newer Claude's: the subagent's own file, beside it.
        std::fs::create_dir_all(dir.path().join("c1/subagents")).unwrap();
        std::fs::write(
            dir.path().join("c1/subagents/agent-a1.jsonl"),
            line(serde_json::json!({"type": "assistant", "timestamp": "2026-10-25T18:00:00.000Z",
                "message": {"id": "s2", "model": "claude-haiku-5-5", "usage": {"input_tokens": 9, "output_tokens": 60}}})),
        )
        .unwrap();
        // Read once its statistics were asked for (above), from then on.
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!((s.output_tokens, s.context_used), (Some(600), Some(1012)));
        // A forked one's: a copy of the session's reply first, counted once.
        let copy = line(
            serde_json::json!({"type": "assistant", "timestamp": "2026-10-25T18:00:00.000Z",
            "message": {"id": "m1", "model": "claude-opus-5-5", "usage": {"input_tokens": 2, "output_tokens": 500}}}),
        );
        std::fs::write(dir.path().join("c1/subagents/agent-a2.jsonl"), copy).unwrap();
        t.update().unwrap();
        assert_eq!(t.stats(None).output_tokens, Some(600));
        // For its prompts alone, they're not read.
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        assert!(t.subagents.is_empty());
    }

    #[test]
    fn a_fork_counts_what_it_spent_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.jsonl");
        let forked = |line: String| {
            line.replacen(
                '{',
                r#"{"forkedFrom":{"sessionId":"p","messageUuid":"u"},"#,
                1,
            )
        };
        // The copy, each line marked, then its own.
        let mut text = forked(prompt("in the parent"));
        text += &forked(reply("m1", 500, 1000));
        text += &prompt("in the fork");
        text += &reply("m2", 40, 3000);
        std::fs::write(&path, &text).unwrap();
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_used),
            (Some(1), Some(40), Some(3012))
        );
        assert_eq!(t.recent_prompts(), ["in the fork"]);
    }

    #[test]
    fn a_copy_roster_made_counts_what_it_spent_itself() {
        // --fork-session writes the copy unmarked, each line with its uuid,
        // as dated then; and carries the conversation's cost over (Claude
        // 2.1.311, as seen in a copy's cost-state: the parent's output and
        // cost in it, its start time).
        let dir = tempfile::tempdir().unwrap();
        let with = |line: String, uuid: &str, ts: &str| {
            line.replacen('{', &format!(r#"{{"uuid":"{uuid}","timestamp":"{ts}","#), 1)
        };
        let cost = |usd: f64, added: u64| {
            line(
                serde_json::json!({"type": "cost-state", "totalCostUSD": usd,
                "totalLinesAdded": added, "totalLinesRemoved": 1}),
            )
        };
        let parent = [
            with(prompt("in the parent"), "u1", "2026-10-25T10:00:00.000Z"),
            with(reply("m1", 500, 1000), "u2", "2026-10-25T10:00:05.000Z"),
            cost(1.5, 10),
        ]
        .concat();
        // Gone on after the copy was made.
        let later = [
            with(prompt("later"), "u9", "2026-10-25T13:00:00.000Z"),
            cost(4.0, 30),
        ]
        .concat();
        std::fs::write(dir.path().join("p.jsonl"), parent.clone() + &later).unwrap();
        // Its own first prompt, dated before the session (a start that
        // failed, a clock stepped back) is its own still.
        let copy = [
            with(prompt("in the parent"), "u1", "2026-10-25T10:00:00.000Z"),
            with(reply("m1", 500, 1000), "u2", "2026-10-25T10:00:05.000Z"),
            with(prompt("in the copy"), "u3", "2026-10-25T10:30:00.000Z"),
            with(reply("m2", 40, 3000), "u4", "2026-10-25T12:00:05.000Z"),
            cost(2.25, 15),
        ]
        .concat();
        let path = dir.path().join("c.jsonl");
        std::fs::write(&path, &copy).unwrap();
        let made = crate::util::iso_ms("2026-10-25T11:00:00.000Z").unwrap();
        let read = |from: &str| {
            let mut t = ClaudeTranscript::new(&path);
            t.forked(from, made);
            t.update().unwrap();
            t
        };
        let t = read("p");
        let s = t.stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.context_used),
            (Some(1), Some(40), Some(3012))
        );
        assert_eq!(
            (s.cost_usd, s.lines_added, s.lines_removed),
            (Some(0.75), Some(5), Some(0))
        );
        // Read before Claude wrote the copy (it does with its first
        // message), the conversation gaining meanwhile what it then copied.
        std::fs::write(&path, "").unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(
            dir.path().join("p.jsonl"),
            &parent[..parent.find(r#""uuid":"u2""#).unwrap() - 1],
        )
        .unwrap();
        let mut early = ClaudeTranscript::new(&path);
        early.forked("p", made);
        assert!(early.update().is_err());
        std::fs::write(dir.path().join("p.jsonl"), parent.clone() + &later).unwrap();
        std::fs::write(&path, &copy).unwrap();
        early.update().unwrap();
        let s = early.stats(None);
        assert_eq!((s.prompts, s.output_tokens), (Some(1), Some(40)));
        // Rewritten by Claude (compacted): still told apart.
        let mut t = t;
        std::fs::write(&path, &copy[..copy.find(r#""uuid":"u4""#).unwrap() - 1]).unwrap();
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!((s.prompts, s.output_tokens), (Some(1), None));
        std::fs::write(&path, &copy).unwrap();
        // The conversation it came from gone: told by the time it was made,
        // and what it cost itself unknown.
        let s = read("gone").stats(None);
        assert_eq!(
            (s.prompts, s.output_tokens, s.cost_usd, s.lines_added),
            (Some(0), Some(40), None, None)
        );
        // Only a file beside it: a name with a path in it isn't followed.
        let below = dir.path().join("sub");
        std::fs::create_dir(&below).unwrap();
        std::fs::write(below.join("c.jsonl"), &copy).unwrap();
        let mut t = ClaudeTranscript::new(&below.join("c.jsonl"));
        t.forked("p", made);
        t.update().unwrap();
        assert_eq!(t.stats(None).cost_usd, None);
        let mut t = ClaudeTranscript::new(&below.join("c.jsonl"));
        t.forked("../p", made);
        t.update().unwrap();
        assert_eq!(t.stats(None).cost_usd, None);
        // Not a copy: all of it its own.
        let mut t = ClaudeTranscript::new(&path);
        t.update().unwrap();
        let s = t.stats(None);
        assert_eq!((s.prompts, s.cost_usd), (Some(2), Some(2.25)));
    }

    #[test]
    fn a_parents_cost_is_the_one_before_its_next_message() {
        let cost = |usd| Cost {
            usd,
            added: 0,
            removed: 0,
        };
        let parent = Parent {
            at: HashMap::new(),
            messages: 4,
            costs: vec![
                (0, cost(0.1)),
                (2, cost(1.0)),
                (2, cost(1.5)),
                (4, cost(3.0)),
            ],
        };
        // Its second message (1) the last copied: the last written before
        // its third.
        assert_eq!(parent.cost_until(Some(1)), cost(1.5));
        assert_eq!(parent.cost_until(Some(3)), cost(3.0));
        assert_eq!(parent.cost_until(None), cost(0.1));
        assert_eq!(cost(1.0).since(cost(2.0)), cost(0.0));
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
