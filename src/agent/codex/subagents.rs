//! What Codex keeps of a conversation's subagents: each is a thread of its
//! own, in a rollout of its own beside its parent's
//! (`rollout-<timestamp>-<its id>.jsonl`), its id the `agent_id` its hooks
//! report. Its rollout begins with a copy of its parent's conversation;
//! Codex says where its own lines start (its `session_meta`'s
//! `subagent_history_start_ordinal`, each line numbered with its `ordinal`).
//! Its task is its place among its parent's agents (`/root/count_files`):
//! what it was told, Codex keeps encrypted.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::agent::{SubagentInfo, clean, host, mcp, tail_lines};

/// How much of a subagent's rollout is read for its first line, which has
/// Codex's instructions in it.
const HEAD: u64 = 4 << 20;

/// How much of its end is read for its model and its latest step: a call is
/// short, what it gave back may not be.
const TAIL: u64 = 256 << 10;

/// How much of its end is read for whether its turn is over: the end is the
/// last thing Codex writes of it, so one further back is a turn at work.
const END_TAIL: u64 = 64 << 10;

/// Subagent `id`'s rollout: beside its parent's at `transcript` (that day's
/// directory), else in the day's it was made on, which its id says (a UUIDv7:
/// when), as codex files it by local date, or a day either side (the clock
/// or the zone changed since); a parent resumed days later has subagents
/// from days its own rollout isn't in.
fn file(transcript: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    let within = |dir: &Path| {
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                (p.file_name().and_then(|n| n.to_str()))
                    .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(&suffix))
            })
    };
    let day = transcript.parent()?;
    if let Some(found) = within(day) {
        return Some(found);
    }
    let sessions = day.parent()?.parent()?.parent()?;
    let made = super::tokens::uuid_ms(id)?;
    const DAY: i64 = 24 * 60 * 60 * 1000;
    [made, made - DAY, made + DAY]
        .into_iter()
        .filter_map(crate::util::local_date)
        .map(|(y, m, d)| sessions.join(format!("{y:04}/{m:02}/{d:02}")))
        .filter(|dir| dir != day)
        .find_map(|dir| within(&dir))
}

/// What a subagent's rollout says of it first.
#[derive(Deserialize)]
struct Meta {
    /// Its place among its parent's agents: `/root/count_files`.
    agent_path: Option<String>,
    /// Where its own lines start, after the copy of its parent's.
    subagent_history_start_ordinal: Option<u64>,
}

/// Its rollout's first line, once it's written.
fn meta(file: &Path) -> Option<Meta> {
    #[derive(Deserialize)]
    struct Line {
        #[serde(rename = "type")]
        kind: String,
        payload: Meta,
    }
    let mut first = Vec::new();
    let head = fs::File::open(file).ok()?.take(HEAD);
    BufReader::new(head).read_until(b'\n', &mut first).ok()?;
    let line: Line = serde_json::from_slice(&first).ok()?;
    (line.kind == "session_meta").then_some(line.payload)
}

/// Its own lines in the last `bytes` of its rollout that may say what's
/// asked, first to last: from `start` on (all of them where Codex doesn't
/// say where they start, as before 0.16x).
fn own_tail(file: &Path, bytes: u64, start: Option<u64>) -> Vec<Value> {
    const MARKS: &[&[u8]] = &[
        br#""turn_context""#,
        br#""thread_settings_applied""#,
        br#""task_started""#,
        br#""task_complete""#,
        br#""turn_aborted""#,
        br#""function_call""#,
        br#""custom_tool_call""#,
        br#""web_search_call""#,
        br#""item_completed""#,
    ];
    (tail_lines(file, bytes).unwrap_or_default().into_iter())
        .filter(|line| {
            MARKS
                .iter()
                .any(|m| memchr::memmem::find(line, m).is_some())
        })
        .filter_map(|line| serde_json::from_slice::<Value>(&line).ok())
        .filter(|v| start.is_none_or(|s| v["ordinal"].as_u64().is_some_and(|o| o >= s)))
        .collect()
}

/// A line's kind: its `type`, and its payload's.
fn kind(v: &Value) -> (&str, &str) {
    fn text(v: &Value) -> &str {
        v.as_str().unwrap_or("")
    }
    (text(&v["type"]), text(&v["payload"]["type"]))
}

/// Subagent `id`'s task and model, in the conversation whose rollout is at
/// `transcript`: all there once its first line is read and it has started
/// a turn.
pub(super) fn info(transcript: &Path, id: &str) -> SubagentInfo {
    let Some(file) = file(transcript, id) else {
        return SubagentInfo::default();
    };
    let meta = meta(&file);
    let start = meta.as_ref().and_then(|m| m.subagent_history_start_ordinal);
    let task = (meta.as_ref().and_then(|m| m.agent_path.as_deref()))
        .and_then(|path| path.rsplit('/').next())
        .filter(|task| !task.trim().is_empty())
        .map(String::from);
    let model = (own_tail(&file, TAIL, start).iter().rev()).find_map(|v| {
        let model = match kind(v) {
            ("turn_context", _) => &v["payload"]["model"],
            ("event_msg", "thread_settings_applied") => &v["payload"]["thread_settings"]["model"],
            _ => return None,
        };
        model.as_str().filter(|m| !m.is_empty()).map(String::from)
    });
    SubagentInfo {
        task,
        settled: meta.is_some() && model.is_some(),
        model,
        file: Some(file),
    }
}

/// Whether subagent `id` is finished: its own turn ended (complete or
/// interrupted), written since it was last heard of (`heard`, ms), not one
/// before it was given another. Not where Codex doesn't say which lines are
/// its own: its copy of its parent's has turns ended too.
pub(super) fn finished(transcript: &Path, id: &str, heard: i64) -> bool {
    let Some(file) = file(transcript, id) else {
        return false;
    };
    let written = (fs::metadata(&file).and_then(|m| m.modified()).ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |t| t.as_millis() as i64);
    if written < heard {
        return false;
    }
    let Some(start) = meta(&file).and_then(|m| m.subagent_history_start_ordinal) else {
        return false;
    };
    (own_tail(&file, END_TAIL, Some(start)).iter().rev())
        .find_map(|v| match kind(v) {
            ("event_msg", "task_started") => Some(false),
            ("event_msg", "task_complete" | "turn_aborted") => Some(true),
            _ => None,
        })
        .unwrap_or(false)
}

/// What a subagent is doing now, from (the end of) its rollout at `file`:
/// its latest step, in a few words, as Codex's own screen has it; none
/// before its first, or where what one gave back is larger than what's read
/// (`TAIL`), which the caller keeps the step it had over.
pub(super) fn doing(file: &Path) -> Option<String> {
    let start = meta(file).and_then(|m| m.subagent_history_start_ordinal);
    (own_tail(file, TAIL, start).iter().rev()).find_map(step)
}

/// A step: a command run, a file changed, a search; or, as Codex writes
/// them before they're done, the call that makes one (in its code mode,
/// the code its tools are run from, which names the command). None for a
/// line that's no step.
fn step(v: &Value) -> Option<String> {
    let p = &v["payload"];
    let said = match kind(v) {
        ("event_msg", "item_completed") => item(&p["item"])?,
        ("response_item", "custom_tool_call") => match p["name"].as_str()? {
            "exec" => code(p["input"].as_str()?)?,
            name => name.to_string(),
        },
        ("response_item", "function_call") => {
            let args: Value = (p["arguments"].as_str())
                .and_then(|a| serde_json::from_str(a).ok())
                .unwrap_or_default();
            call(p["name"].as_str()?, &args)?
        }
        ("response_item", "web_search_call") => search(&p["action"])?,
        _ => return None,
    };
    let said = clean(&said);
    (!said.is_empty()).then_some(said)
}

/// What a finished item did; None for one that's no step (a reply, its
/// reasoning).
fn item(item: &Value) -> Option<String> {
    fn text(v: &Value) -> Option<&str> {
        v.as_str().map(str::trim).filter(|s| !s.is_empty())
    }
    match item["type"].as_str()? {
        "CommandExecution" => {
            let parsed = item["parsed_cmd"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            // What a command line is for, of its commands the first that
            // says (`cd x && cat y` is reading y).
            let part = (parsed.iter().find(|c| c["type"] != "unknown")).or(parsed.first());
            let command = || {
                (item["command"].as_array().and_then(|c| c.last()))
                    .and_then(text)
                    .map(running)
            };
            let Some(part) = part else {
                return command();
            };
            match part["type"].as_str()? {
                "read" => text(&part["name"]).map(|f| format!("Reading {f}")),
                "list_files" => Some(match text(&part["path"]) {
                    Some(path) => format!("Listing files in {path}"),
                    None => "Listing files".into(),
                }),
                "search" => text(&part["query"]).map(|q| format!("Searching for {q}")),
                _ => text(&part["cmd"]).map(running),
            }
            .or_else(command)
        }
        "FileChange" => {
            let changes = item["changes"].as_object()?;
            let (path, change) = match changes.len() {
                1 => changes.iter().next()?,
                n => return Some(format!("Editing {n} files")),
            };
            let name = path.rsplit('/').next().unwrap_or(path);
            Some(match change["type"].as_str() {
                Some("add") => format!("Writing {name}"),
                Some("delete") => format!("Deleting {name}"),
                _ => format!("Editing {name}"),
            })
        }
        "Extension" => match item["kind"].as_str()? {
            "web.search" => search(&item["action"]),
            kind => Some(kind.to_string()),
        },
        "McpToolCall" => match (text(&item["server"]), text(&item["tool"])) {
            (Some(server), Some(tool)) => Some(format!("{tool} ({server})")),
            _ => None,
        },
        _ => None,
    }
}

/// A web search, or a page it opened (by its host alone, never a login or a
/// token in its URL).
fn search(action: &Value) -> Option<String> {
    let text = |key: &str| {
        action[key]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    match action["type"].as_str() {
        Some("open_page") => text("url").map(|u| format!("Opening {}", host(u))),
        Some("find_in_page") => text("pattern").map(|p| format!("Finding {p}")),
        _ => text("query").map(|q| format!("Searching the web for {q}")),
    }
}

/// A call made outside its code mode, by its name and arguments; None for
/// waiting on code already running (its code says what).
fn call(name: &str, args: &Value) -> Option<String> {
    let text = |key: &str| args[key].as_str().map(str::trim).filter(|s| !s.is_empty());
    let said = match name {
        "wait" => return None,
        "exec_command" => text("cmd").map(running),
        "shell" | "shell_command" | "local_shell" => match &args["command"] {
            Value::Array(parts) => parts.last().and_then(Value::as_str).map(running),
            Value::String(command) => Some(running(command)),
            _ => None,
        },
        "apply_patch" => Some("Editing files".into()),
        "spawn_agent" => text("task_name").map(|t| format!("Starting {t}")),
        "wait_agent" => Some("Waiting for agents".into()),
        "view_image" => {
            text("path").map(|p| format!("Viewing {}", p.rsplit('/').next().unwrap_or(p)))
        }
        "update_plan" => (args["plan"].as_array())
            .and_then(|plan| plan.iter().find(|s| s["status"] == "in_progress"))
            .and_then(|s| s["step"].as_str())
            .map(String::from),
        _ => mcp(name),
    };
    Some(said.unwrap_or_else(|| name.to_string()))
}

/// A command, by its first line.
fn running(command: &str) -> String {
    format!("Running {}", command.trim().lines().next().unwrap_or(""))
}

/// What a piece of Codex's code mode does: the command it runs, else the
/// first of its tools it calls; None for one that only waits on a command
/// already running (`write_stdin`), which the call that ran it says, or
/// calls none.
fn code(code: &str) -> Option<String> {
    if let Some(command) = js_cmd(code) {
        return Some(running(&command));
    }
    let (_, rest) = code.split_once("tools.")?;
    let name: String = (rest.chars())
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    match name.as_str() {
        "" | "write_stdin" => None,
        name => call(name, &Value::Null),
    }
}

/// The command a piece of Codex's code mode runs: the first string given
/// as `cmd` (`tools.exec_command({cmd: "cargo test"})`).
fn js_cmd(code: &str) -> Option<String> {
    let mut rest = code;
    while let Some(at) = rest.find("cmd") {
        rest = &rest[at + 3..];
        let value = rest.trim_start_matches(['"', '\'']).trim_start();
        let Some(value) = value.strip_prefix(':').map(str::trim_start) else {
            continue;
        };
        if let Some(quote) = value
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\'' | '`'))
        {
            return literal(&value[1..], quote);
        }
    }
    None
}

/// A string literal's text, up to its closing `quote`.
fn literal(s: &str, quote: char) -> Option<String> {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                c => out.push(c),
            },
            c if c == quote => return Some(out),
            c => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const PARENT: &str = "01a17d3b-4a00-7000-8000-000000000001";
    const ID: &str = "01a17d3b-55b8-7000-8000-000000000002";

    /// A subagent's rollout as Codex 0.164 writes it: its meta, a copy of its
    /// parent's (with a turn of it ended), then its own lines (`own`), from
    /// ordinal 3.
    fn rollout(own: &[Value]) -> String {
        let meta = json!({"type": "session_meta", "payload": {
            "id": ID, "parent_thread_id": PARENT, "agent_path": "/root/count_files",
            "agent_nickname": "Sagan", "subagent_history_start_ordinal": 3}});
        let copy = [
            json!({"ordinal": 1, "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
            json!({"ordinal": 2, "type": "event_msg", "payload": {"type": "turn_aborted"}}),
        ];
        let numbered = own.iter().enumerate().map(|(i, line)| {
            let mut line = line.clone();
            line["ordinal"] = json!(3 + i);
            line
        });
        let lines: Vec<String> = [meta]
            .into_iter()
            .chain(copy)
            .chain(numbered)
            .map(|l| l.to_string())
            .collect();
        lines.join("\n") + "\n"
    }

    fn settings(model: &str) -> Value {
        json!({"type": "event_msg", "payload": {"type": "thread_settings_applied",
            "thread_id": ID, "thread_settings": {"model": model}}})
    }

    fn event(kind: &str) -> Value {
        json!({"type": "event_msg", "payload": {"type": kind}})
    }

    fn command(parsed: Value) -> Value {
        json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {
            "type": "CommandExecution", "command": ["/usr/bin/zsh", "-lc", "the line"],
            "parsed_cmd": parsed}}})
    }

    /// A parent's rollout in a day's directory, and where its subagent's goes:
    /// the day it was made, in local time.
    fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let made = super::super::tokens::uuid_ms(ID).unwrap();
        let (y, m, d) = crate::util::local_date(made).unwrap();
        let day = dir.path().join(format!("sessions/{y:04}/{m:02}/{d:02}"));
        fs::create_dir_all(&day).unwrap();
        let parent = day.join(format!("rollout-2026-10-27T12-00-00-{PARENT}.jsonl"));
        fs::write(&parent, "").unwrap();
        let sub = day.join(format!("rollout-2026-10-27T12-00-03-{ID}.jsonl"));
        (dir, parent, sub)
    }

    #[test]
    fn a_subagents_task_and_model_come_from_its_own_lines() {
        let (dir, parent, sub) = dirs();
        assert_eq!(info(&parent, ID), SubagentInfo::default());
        // Its copy of its parent's isn't its own: no model yet.
        fs::write(&sub, rollout(&[])).unwrap();
        let i = info(&parent, ID);
        let some = |s: &str| Some(s.to_string());
        assert_eq!(
            (i.task, i.model, i.settled),
            (some("count_files"), None, false)
        );
        assert_eq!(i.file.as_deref(), Some(sub.as_path()));
        // Its settings, then the turn it runs.
        fs::write(&sub, rollout(&[settings("gpt-6-luna")])).unwrap();
        assert_eq!(info(&parent, ID).model, some("gpt-6-luna"));
        let turn = json!({"type": "turn_context", "payload": {"model": "gpt-6-terra"}});
        fs::write(&sub, rollout(&[settings("gpt-6-luna"), turn])).unwrap();
        let i = info(&parent, ID);
        assert_eq!((i.model, i.settled), (some("gpt-6-terra"), true));
        // Its parent resumed days later, in another day's directory, and
        // other days' since: found by the day its id says it was made.
        let earlier = dir.path().join("sessions/2026/10/20");
        fs::create_dir_all(&earlier).unwrap();
        let resumed = earlier.join(parent.file_name().unwrap());
        fs::rename(&parent, &resumed).unwrap();
        let later = dir.path().join("sessions/2026/11/30");
        fs::create_dir_all(&later).unwrap();
        fs::write(later.join("rollout-2026-11-30T09-00-00-other.jsonl"), "").unwrap();
        assert_eq!(info(&resumed, ID).task, some("count_files"));
        let ended = [
            settings("gpt-6-luna"),
            event("task_started"),
            event("task_complete"),
        ];
        fs::write(&sub, rollout(&ended)).unwrap();
        assert!(finished(&resumed, ID, 0));
        // An id that isn't a UUIDv7 says no day: beside its parent's only.
        assert_eq!(info(&resumed, "not-a-uuid"), SubagentInfo::default());
    }

    #[test]
    fn a_subagent_is_finished_once_its_own_turn_ends() {
        let (_dir, parent, sub) = dirs();
        assert!(!finished(&parent, ID, 0));
        // The copy's ended turn isn't its own.
        fs::write(&sub, rollout(&[settings("gpt-6-luna")])).unwrap();
        assert!(!finished(&parent, ID, 0));
        fs::write(&sub, rollout(&[event("task_started")])).unwrap();
        assert!(!finished(&parent, ID, 0));
        for end in ["task_complete", "turn_aborted"] {
            fs::write(&sub, rollout(&[event("task_started"), event(end)])).unwrap();
            assert!(finished(&parent, ID, 0), "{end}");
        }
        // Given another task since: at work again.
        let again = [
            event("task_started"),
            event("task_complete"),
            event("task_started"),
        ];
        fs::write(&sub, rollout(&again)).unwrap();
        assert!(!finished(&parent, ID, 0));
        // An end from before it was last heard of isn't this one's.
        fs::write(&sub, rollout(&[event("task_complete")])).unwrap();
        assert!(!finished(&parent, ID, crate::util::now_ms() + 60_000));
        // Without where its own lines start, never.
        let old = json!({"type": "session_meta", "payload": {"id": ID}}).to_string()
            + "\n"
            + &event("task_complete").to_string()
            + "\n";
        fs::write(&sub, old).unwrap();
        assert!(!finished(&parent, ID, 0));
    }

    #[test]
    fn what_a_subagent_is_doing_is_its_latest_step() {
        let (_dir, _parent, sub) = dirs();
        let doing_of = |own: &[Value]| {
            fs::write(&sub, rollout(own)).unwrap();
            doing(&sub)
        };
        let some = |s: &str| Some(s.to_string());
        // The copy's steps aren't its own.
        assert_eq!(doing_of(&[settings("gpt-6-luna")]), None);
        // Its code at work, by the command it runs; once done, what it was.
        let code = |input: &str| {
            json!({"type": "response_item", "payload": {"type": "custom_tool_call",
                "name": "exec", "input": input}})
        };
        let exec = code(
            r#"const r = await tools.exec_command({cmd:"cargo test\nmore", max_output_tokens:2000});"#,
        );
        assert_eq!(
            doing_of(std::slice::from_ref(&exec)),
            some("Running cargo test")
        );
        let quoted = code(r#"await tools.exec_command({"cmd": 'echo "it\'s"'})"#);
        assert_eq!(doing_of(&[quoted]), some(r#"Running echo "it's""#));
        // Waiting on the command it ran says that command; another tool, by
        // its name.
        let poll =
            code("const r = await tools.write_stdin({session_id:26246, yield_time_ms:30000});");
        assert_eq!(
            doing_of(&[exec.clone(), poll.clone()]),
            some("Running cargo test")
        );
        assert_eq!(doing_of(&[poll, code("text(1)")]), None);
        let patch = code("await tools.apply_patch(\"*** Begin Patch\")");
        assert_eq!(doing_of(&[patch]), some("Editing files"));
        let read = command(json!([{"type": "read", "cmd": "cat src/a.rs", "name": "a.rs"}]));
        assert_eq!(doing_of(&[exec.clone(), read]), some("Reading a.rs"));
        let list = command(
            json!([{"type": "unknown", "cmd": "cd x"}, {"type": "list_files", "cmd": "ls", "path": null}]),
        );
        assert_eq!(doing_of(&[list]), some("Listing files"));
        let search = command(json!([{"type": "search", "cmd": "rg foo", "query": "foo"}]));
        assert_eq!(doing_of(&[search]), some("Searching for foo"));
        let other = command(json!([{"type": "unknown", "cmd": "make it"}]));
        assert_eq!(doing_of(&[other]), some("Running make it"));
        assert_eq!(doing_of(&[command(json!([]))]), some("Running the line"));
        // A reply after a step leaves the step.
        let reply = json!({"type": "event_msg", "payload": {"type": "item_completed",
            "item": {"type": "AgentMessage"}}});
        let edit = json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {
            "type": "FileChange", "changes": {"/w/src/db.rs": {"type": "update"}}}}});
        assert_eq!(doing_of(&[edit, reply]), some("Editing db.rs"));
        let web = json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {
            "type": "Extension", "kind": "web.search",
            "action": {"type": "open_page", "url": "https://me:secret@example.com/x?token=1"}}}});
        assert_eq!(doing_of(&[web]), some("Opening example.com"));
        // Calls outside its code mode; waiting on its code says its code.
        let call = |name: &str, args: Value| {
            json!({"type": "response_item", "payload": {"type": "function_call",
                "name": name, "arguments": args.to_string()}})
        };
        let spawn = call("spawn_agent", json!({"task_name": "find_it"}));
        assert_eq!(doing_of(&[spawn]), some("Starting find_it"));
        let shell = call("shell", json!({"command": ["bash", "-lc", "npm test"]}));
        assert_eq!(doing_of(&[shell]), some("Running npm test"));
        let waited = call("wait", json!({"cell_id": "9"}));
        assert_eq!(doing_of(&[exec, waited]), some("Running cargo test"));
        assert_eq!(
            doing_of(&[call("mcp__docs__search", json!({}))]),
            some("search (docs)")
        );
        // On one line, nothing a terminal would take for more.
        let sly = command(json!([{"type": "unknown", "cmd": "a\u{1b}[2J\u{202e}b"}]));
        assert_eq!(doing_of(&[sly]), some("Running a[2Jb"));
    }
}
