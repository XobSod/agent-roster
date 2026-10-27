//! What Claude keeps of a conversation's subagents beside its transcript
//! (`<id>/subagents/`, a directory or two deeper for some): each one's
//! transcript, `agent-<id>.jsonl`, and what it was started with,
//! `agent-<id>.meta.json`: its type, the description it was given, and the
//! model asked for, if one was.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agent::SubagentInfo;

/// How much of a subagent's transcript is read for the model its first
/// reply came from: its task, then that reply.
const HEAD: u64 = 1 << 20;

/// Subagent `id`'s task and model, in the conversation whose transcript is
/// at `transcript`: the model its replies are from, else the one it was
/// asked to run on. All there once its meta file is read and it has
/// replied.
pub(super) fn info(transcript: &Path, id: &str) -> SubagentInfo {
    let dir = transcript.with_extension("").join("subagents");
    let Some(file) = find(&dir, &format!("agent-{id}.jsonl"), 3) else {
        return SubagentInfo::default();
    };
    let meta: Option<Meta> = fs::read(file.with_extension("meta.json"))
        .ok()
        .and_then(|text| serde_json::from_slice(&text).ok());
    let read = meta.is_some();
    let meta = meta.unwrap_or_default();
    let task = meta.description.filter(|t| !t.trim().is_empty());
    let replied = first_model(&file);
    SubagentInfo {
        task,
        settled: read && replied.is_some(),
        model: replied.or(meta.model.filter(|m| !m.is_empty())),
        file: Some(file),
    }
}

/// Whether subagent `id` is finished: its meta file says so (Claude marks
/// it as a subagent ends, since 2.1.301), written since it was last heard
/// of (`heard`, ms), not left from a run it was taken up again after.
pub(super) fn finished(transcript: &Path, id: &str, heard: i64) -> bool {
    let dir = transcript.with_extension("").join("subagents");
    let Some(file) = find(&dir, &format!("agent-{id}.meta.json"), 3) else {
        return false;
    };
    #[derive(Deserialize)]
    struct Done {
        #[serde(default)]
        finished: bool,
    }
    let written = (fs::metadata(&file).and_then(|m| m.modified()).ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |t| t.as_millis() as i64);
    written >= heard
        && fs::read(&file)
            .ok()
            .and_then(|text| serde_json::from_slice::<Done>(&text).ok())
            .is_some_and(|d| d.finished)
}

/// How much of the end of a subagent's transcript is read for its latest
/// step: a tool's call is short, its result (which comes after) may not be.
const TAIL: u64 = 256 << 10;

/// What a subagent is doing now, from (the end of) its transcript at
/// `file`: its latest tool call, in a few words, in the manner of Claude's
/// own footer (which it writes nowhere); none before its first, or where
/// a result larger than what's read follows it (`TAIL`), which the caller
/// keeps the step it had over.
pub(super) fn doing(file: &Path) -> Option<String> {
    let mut f = fs::File::open(file).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "type")]
        kind: Option<String>,
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    struct Message {
        content: Option<serde_json::Value>,
    }
    // From the end, the first call found; a line cut where the tail starts
    // isn't one. Claude writes a row per block, a call's once it's whole.
    let mut lines: Vec<&[u8]> = tail.split(|b| *b == b'\n').collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
        .into_iter()
        .rev()
        .filter(|line| memchr::memmem::find(line, br#""tool_use""#).is_some())
        .filter_map(|line| serde_json::from_slice::<Row>(line).ok())
        .filter(|row| row.kind.as_deref() == Some("assistant"))
        .filter_map(|row| row.message?.content)
        .find_map(|content| match content {
            serde_json::Value::Array(blocks) => blocks
                .into_iter()
                .rev()
                .find(|block| block["type"] == "tool_use"),
            _ => None,
        })
        .as_ref()
        .and_then(step)
}

/// How long a step is let be: a line on a card, and a bit to cut.
const STEP_CHARS: usize = 200;

/// A tool call, in a few words: what it's for where the call says (a
/// command's description, a todo at work), else what it's on. Claude's
/// footer has a command as "Running <description>", a file by its path, a
/// fetch by its URL; here they're shorter, the card narrow.
fn step(call: &serde_json::Value) -> Option<String> {
    let input = &call["input"];
    let text = |key: &str| input[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    // By its name; a path ending in a slash, as it is.
    let file = |key: &str| {
        text(key).map(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            if name.is_empty() { path } else { name }.to_string()
        })
    };
    let name = call["name"].as_str()?;
    let said = match name {
        "Bash" => text("description").map(String::from).or_else(|| {
            text("command").map(|c| format!("Running {}", c.lines().next().unwrap_or(c)))
        }),
        "Read" => file("file_path").map(|f| format!("Reading {f}")),
        "Write" => file("file_path").map(|f| format!("Writing {f}")),
        "Edit" | "MultiEdit" => file("file_path").map(|f| format!("Editing {f}")),
        "NotebookEdit" => file("notebook_path").map(|f| format!("Editing {f}")),
        "Grep" => text("pattern").map(|p| format!("Searching for {p}")),
        "Glob" => text("pattern").map(|p| format!("Finding {p}")),
        "WebFetch" => text("url").map(|u| format!("Fetching {}", host(u))),
        "WebSearch" => text("query").map(|q| format!("Searching the web for {q}")),
        "Agent" | "Task" => text("description").map(String::from),
        "Monitor" => text("description").map(|d| format!("Monitoring: {d}")),
        "SendMessage" => text("summary").map(String::from),
        "Skill" => text("skill").map(|s| format!("Using {s}")),
        "ToolSearch" => text("query").map(|q| format!("Finding tools: {q}")),
        "ScheduleWakeup" => text("reason").map(String::from),
        "TaskCreate" => text("activeForm").or(text("subject")).map(String::from),
        "TaskUpdate" => text("activeForm")
            .filter(|_| input["status"] == "in_progress")
            .map(String::from),
        "TodoWrite" => (input["todos"].as_array())
            .and_then(|todos| todos.iter().find(|t| t["status"] == "in_progress"))
            .and_then(|t| (t["activeForm"].as_str()).or(t["content"].as_str()))
            .map(String::from),
        _ => mcp(name),
    };
    let said = clean(&said.unwrap_or_else(|| name.to_string()));
    (!said.is_empty()).then_some(said)
}

/// A fetched URL's host, nothing else of it: not a login before an `@`, nor
/// a query (a token, say).
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    authority.rsplit('@').next().unwrap_or(authority)
}

/// An MCP server's tool (`mcp__<server>__<tool>`) as `<tool> (<server>)`.
fn mcp(name: &str) -> Option<String> {
    let (server, tool) = name.strip_prefix("mcp__")?.split_once("__")?;
    Some(format!("{tool} ({server})"))
}

/// `said` on one line, without what a terminal would take for more than
/// text (control characters, which a card would show the rest of, and
/// direction overrides), at most `STEP_CHARS` long.
fn clean(said: &str) -> String {
    let bidi = |c: char| matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let kept: String = (said.chars())
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !bidi(*c))
        .collect();
    crate::util::one_line(&kept)
        .chars()
        .take(STEP_CHARS)
        .collect()
}

#[derive(Default, Deserialize)]
struct Meta {
    description: Option<String>,
    model: Option<String>,
}

/// The file named `name` in `dir`, or in its directories `depth` deep (not
/// linked ones, which could loop).
fn find(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    let here = dir.join(name);
    if here.is_file() {
        return Some(here);
    }
    if depth == 0 {
        return None;
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .find_map(|e| find(&e.path(), name, depth - 1))
}

/// The model of the first reply in (the head of) a subagent's transcript:
/// after its task, a forked one's file having before it a copy of the reply
/// it was forked from.
fn first_model(file: &Path) -> Option<String> {
    let mut head = Vec::new();
    fs::File::open(file)
        .ok()?
        .take(HEAD)
        .read_to_end(&mut head)
        .ok()?;
    #[derive(Deserialize)]
    struct Row {
        #[serde(rename = "type")]
        kind: Option<String>,
        message: Option<Message>,
    }
    #[derive(Deserialize)]
    struct Message {
        model: Option<String>,
    }
    let rows =
        (head.split(|b| *b == b'\n')).filter_map(|line| serde_json::from_slice::<Row>(line).ok());
    let mut begun = false;
    for row in rows {
        match row.kind.as_deref() {
            Some("user") => begun = true,
            Some("assistant") if begun => {
                if let Some(model) = row.message.and_then(|m| m.model)
                    && model != "<synthetic>"
                {
                    return Some(model);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subagents_task_and_model_come_from_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("c1.jsonl");
        let runs = dir.path().join("c1/subagents/workflows");
        fs::create_dir_all(&runs).unwrap();
        let reply = |model: &str| {
            format!(r#"{{"type":"assistant","message":{{"model":"{model}","content":[]}}}}"#)
        };
        let task = r#"{"type":"user","message":{"role":"user","content":"look for the model"}}"#;
        let write = |id: &str, lines: &[&str], meta: Option<&str>| {
            fs::write(
                runs.join(format!("agent-{id}.jsonl")),
                lines.join("\n") + "\n",
            )
            .unwrap();
            if let Some(meta) = meta {
                fs::write(runs.join(format!("agent-{id}.meta.json")), meta).unwrap();
            }
        };
        let show = |id: &str| {
            let i = super::info(&transcript, id);
            (i.task, i.model, i.settled)
        };
        let some = |s: &str| Some(s.to_string());
        // Asked for by name: that, till it replies; then what it replied with.
        let meta = r#"{"agentType":"general-purpose","description":"Review it","model":"sonnet"}"#;
        write("a1", &[task], Some(meta));
        assert_eq!(show("a1"), (some("Review it"), some("sonnet"), false));
        write("a1", &[task, &reply("claude-sonnet-5-5")], Some(meta));
        assert_eq!(
            show("a1"),
            (some("Review it"), some("claude-sonnet-5-5"), true)
        );
        // Forked: the copy of the reply it came from, before its task, isn't
        // its own; Claude's own messages aren't replies.
        let meta = r#"{"agentType":"fork","description":"Fix it","model":"inherit"}"#;
        let copy = reply("claude-opus-5-5");
        write("a2", &[&copy, task, &reply("<synthetic>")], Some(meta));
        assert_eq!(show("a2"), (some("Fix it"), some("inherit"), false));
        write(
            "a2",
            &[&copy, task, &reply("claude-haiku-4-5-20251001")],
            Some(meta),
        );
        assert_eq!(show("a2").1, some("claude-haiku-4-5-20251001"));
        // Its meta not there (yet): not all there; one with no description,
        // no task.
        write("a3", &[task, &reply("claude-opus-5-5")], None);
        assert_eq!(show("a3"), (None, some("claude-opus-5-5"), false));
        write(
            "a3",
            &[task, &reply("claude-opus-5-5")],
            Some(r#"{"description":" "}"#),
        );
        assert_eq!(show("a3"), (None, some("claude-opus-5-5"), true));
        assert_eq!(super::info(&transcript, "gone"), SubagentInfo::default());
    }

    #[test]
    fn what_a_subagent_is_doing_is_its_latest_step() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agent-a1.jsonl");
        let call = |name: &str, input: serde_json::Value| {
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "text", "text": "next"},
                {"type": "tool_use", "name": name, "input": input}]}})
            .to_string()
        };
        let result =
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"done"}]}}"#;
        let doing = |lines: &[String]| {
            fs::write(&file, lines.join("\n") + "\n").unwrap();
            super::doing(&file)
        };
        let task = r#"{"type":"user","message":{"content":"go"}}"#.to_string();
        assert_eq!(doing(std::slice::from_ref(&task)), None);
        let bash = call(
            "Bash",
            serde_json::json!({"command": "cargo test -q", "description": "Run the tests"}),
        );
        // Its latest, finished or not.
        assert_eq!(
            doing(&[task.clone(), bash.clone(), result.into()]).as_deref(),
            Some("Run the tests")
        );
        let cases = [
            (
                "Bash",
                serde_json::json!({"command": "ls -la\nmore"}),
                "Running ls -la",
            ),
            (
                "Read",
                serde_json::json!({"file_path": "/w/src/db.rs"}),
                "Reading db.rs",
            ),
            (
                "Edit",
                serde_json::json!({"file_path": "/w/src/x.rs"}),
                "Editing x.rs",
            ),
            (
                "Grep",
                serde_json::json!({"pattern": "fold_events"}),
                "Searching for fold_events",
            ),
            (
                "WebFetch",
                serde_json::json!({"url": "https://example.com/a/b"}),
                "Fetching example.com",
            ),
            (
                "TodoWrite",
                serde_json::json!({"todos": [
                    {"content": "a", "status": "completed", "activeForm": "Doing a"},
                    {"content": "b", "status": "in_progress", "activeForm": "Doing b"}]}),
                "Doing b",
            ),
            (
                "Write",
                serde_json::json!({"file_path": "/w/new.md"}),
                "Writing new.md",
            ),
            (
                "MultiEdit",
                serde_json::json!({"file_path": "/w/m.rs"}),
                "Editing m.rs",
            ),
            (
                "NotebookEdit",
                serde_json::json!({"notebook_path": "/w/n.ipynb"}),
                "Editing n.ipynb",
            ),
            (
                "Read",
                serde_json::json!({"file_path": "/w/dir/"}),
                "Reading /w/dir/",
            ),
            (
                "Glob",
                serde_json::json!({"pattern": "**/*.rs"}),
                "Finding **/*.rs",
            ),
            (
                "WebSearch",
                serde_json::json!({"query": "tmux 3.5"}),
                "Searching the web for tmux 3.5",
            ),
            (
                "Agent",
                serde_json::json!({"description": "Review it"}),
                "Review it",
            ),
            (
                "Monitor",
                serde_json::json!({"description": "the build"}),
                "Monitoring: the build",
            ),
            (
                "SendMessage",
                serde_json::json!({"summary": "Ask the fork"}),
                "Ask the fork",
            ),
            ("Skill", serde_json::json!({"skill": "kb"}), "Using kb"),
            (
                "ToolSearch",
                serde_json::json!({"query": "select:Monitor"}),
                "Finding tools: select:Monitor",
            ),
            (
                "TaskUpdate",
                serde_json::json!({"status": "in_progress", "activeForm": "Fixing it"}),
                "Fixing it",
            ),
            (
                "TaskUpdate",
                serde_json::json!({"status": "completed", "activeForm": "Fixing it"}),
                "TaskUpdate",
            ),
            (
                "mcp__t3-code__html_render",
                serde_json::json!({}),
                "html_render (t3-code)",
            ),
            ("mcp__thing", serde_json::json!({}), "mcp__thing"),
            // What a terminal would take for more than text, and a login
            // or a token in a URL, don't show.
            (
                "Bash",
                serde_json::json!({"description": "\u{1b}[31mRun\u{1b}[0m the\ttests\u{202e}"}),
                "[31mRun[0m the tests",
            ),
            (
                "WebFetch",
                serde_json::json!({"url": "https://u:p4ss@example.com:8443?tok=SECRET#x"}),
                "Fetching example.com:8443",
            ),
        ];
        for (name, input, said) in cases {
            let lines = [task.clone(), bash.clone(), call(name, input)];
            assert_eq!(doing(&lines).as_deref(), Some(said), "{name}");
        }
        // Its last call, though a reply, a half-written row or a second
        // call in the same reply come after.
        let reply = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#;
        let half =
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","inp"#;
        let pair = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Read", "input": {"file_path": "/w/a.rs"}},
            {"type": "tool_use", "name": "Read", "input": {"file_path": "/w/b.rs"}}]}})
        .to_string();
        assert_eq!(
            doing(&[task.clone(), bash.clone(), reply.into()]).as_deref(),
            Some("Run the tests")
        );
        assert_eq!(
            doing(&[task.clone(), bash.clone(), half.into()]).as_deref(),
            Some("Run the tests")
        );
        assert_eq!(
            doing(&[task.clone(), pair]).as_deref(),
            Some("Reading b.rs")
        );
        let long_said = call("Bash", serde_json::json!({"description": "y".repeat(500)}));
        assert_eq!(
            doing(&[task.clone(), long_said]).unwrap().chars().count(),
            STEP_CHARS
        );
        // Past a long result, read from the end: a line cut there isn't one.
        let long = format!(
            r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
            "x".repeat(300 << 10)
        );
        let read = call("Read", serde_json::json!({"file_path": "/w/a.rs"}));
        assert_eq!(doing(&[task, long, read]).as_deref(), Some("Reading a.rs"));
    }

    #[test]
    fn a_subagent_is_finished_once_claude_marks_it_since_last_heard_of() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("c1.jsonl");
        let runs = dir.path().join("c1/subagents");
        fs::create_dir_all(&runs).unwrap();
        let meta = runs.join("agent-a1.meta.json");
        let written = || {
            let t = fs::metadata(&meta).unwrap().modified().unwrap();
            t.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
        };
        assert!(!super::finished(&transcript, "a1", 0));
        fs::write(&meta, r#"{"agentType":"fork","description":"Fix it"}"#).unwrap();
        assert!(!super::finished(&transcript, "a1", 0));
        fs::write(
            &meta,
            r#"{"agentType":"fork","description":"Fix it","finished":true}"#,
        )
        .unwrap();
        assert!(super::finished(&transcript, "a1", written() - 1));
        // Taken up again since: the mark is its last run's.
        assert!(!super::finished(&transcript, "a1", written() + 1000));
    }
}
