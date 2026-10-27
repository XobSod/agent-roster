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

/// How much of the end of a subagent's transcript is read for its latest
/// step: a tool's call is short, its result (which comes after) may not be.
const TAIL: u64 = 256 << 10;

/// What a subagent is doing now, from (the end of) its transcript at
/// `file`: its latest tool call, in a few words, as Claude's own footer
/// says it (which it writes nowhere); none before its first.
pub(super) fn doing(file: &Path) -> Option<String> {
    let mut f = fs::File::open(file).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    f.read_to_end(&mut tail).ok()?;
    // A line cut where the tail starts isn't one.
    let lines = tail.split(|b| *b == b'\n').skip(usize::from(from > 0));
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
    let calls: Vec<serde_json::Value> = lines
        .filter(|line| memchr::memmem::find(line, br#""tool_use""#).is_some())
        .filter_map(|line| serde_json::from_slice::<Row>(line).ok())
        .filter(|row| row.kind.as_deref() == Some("assistant"))
        .filter_map(|row| row.message?.content)
        .flat_map(|content| match content {
            serde_json::Value::Array(blocks) => blocks,
            _ => Vec::new(),
        })
        .filter(|block| block["type"] == "tool_use")
        .collect();
    step(calls.last()?)
}

/// A tool call, in a few words: what it's for where the call says (a
/// command's description, a todo at work), else what it's on.
fn step(call: &serde_json::Value) -> Option<String> {
    let input = &call["input"];
    let text = |key: &str| input[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let file =
        |key: &str| text(key).map(|path| path.rsplit('/').next().unwrap_or(path).to_string());
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
        "WebFetch" => text("url").map(|u| {
            let host = u.split("://").nth(1).unwrap_or(u);
            format!("Fetching {}", host.split('/').next().unwrap_or(host))
        }),
        "WebSearch" => text("query").map(|q| format!("Searching the web for {q}")),
        "Agent" | "Task" => text("description").map(String::from),
        "TodoWrite" => (input["todos"].as_array())
            .and_then(|todos| todos.iter().find(|t| t["status"] == "in_progress"))
            .and_then(|t| (t["activeForm"].as_str()).or(t["content"].as_str()))
            .map(String::from),
        _ => None,
    };
    Some(crate::util::one_line(
        &said.unwrap_or_else(|| name.to_string()),
    ))
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
            ("mcp__thing__go", serde_json::json!({}), "mcp__thing__go"),
        ];
        for (name, input, said) in cases {
            let lines = [task.clone(), bash.clone(), call(name, input)];
            assert_eq!(doing(&lines).as_deref(), Some(said), "{name}");
        }
        // Past a long result, read from the end: a line cut there isn't one.
        let long = format!(
            r#"{{"type":"user","message":{{"content":"{}"}}}}"#,
            "x".repeat(300 << 10)
        );
        let read = call("Read", serde_json::json!({"file_path": "/w/a.rs"}));
        assert_eq!(doing(&[task, long, read]).as_deref(), Some("Reading a.rs"));
    }
}
