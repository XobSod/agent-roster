//! The commands codex leaves running past its turn, its background
//! terminals: `exec_command` given back a session id rather than an exit
//! code, and not polled to its end. No hook says either way, so both are read
//! from the rollout: those running as the turn ends, from that turn's tool
//! outputs; an end, from the record codex writes of each command once it's
//! over (`item_completed`, a `CommandExecution` with that id as its
//! `process_id`), ended by itself or closed with `/stop`. Codex takes none
//! of their results up, so an end is reported by nothing else. It keeps that
//! record only in a thread whose history is paginated (its header's
//! `history_mode`), which a thread begun by an older codex isn't, resumed or
//! forked: there none is taken for left running, as none could be seen to end.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::agent::{Backgrounded, ShellEnd, ShellEnds};

/// How much of a rollout's end is read for its last turn's terminals: a turn
/// longer than that has those started before it left out.
const TAIL: u64 = 4 * 1024 * 1024;

/// What a tool's output says of a command still running, as the plain
/// `exec_command` says it.
const RUNNING: &str = "Process running with session ID ";

/// A command started in the turn.
struct Started {
    /// Codex's id for it.
    id: String,
    /// When its call gave it back running (ms).
    at: Option<i64>,
    /// Its tool call's id, for the command it was given.
    call: Option<String>,
    /// Where the line saying it runs is in the rollout (bytes).
    line: u64,
}

/// The terminals the rollout at `transcript` shows its last turn left
/// running: started in it, with no end recorded anywhere in it. Codex writes
/// a call's output once the response it's in is over, and a command's end
/// as it ends, which can come first, so an output saying one runs (its own
/// call's, a poll's) may come after its end. The rollout as far as `upto`
/// (bytes), if given: as long as it was as that turn ended, rather than a
/// later turn's, read late. Each with when it started, and where it was
/// seen running: its end is looked for after, as before it codex may have
/// given its id to a command over since.
pub(super) fn left(transcript: &Path, upto: Option<u64>) -> Vec<Backgrounded> {
    let Some((tail, offset)) = tail(transcript, upto) else {
        return Vec::new();
    };
    // From the turn's start, or as much of it as was read.
    let turn = memchr::memmem::rfind(&tail, br#""type":"task_started""#).map_or(0, |at| {
        memchr::memrchr(b'\n', &tail[..at]).map_or(0, |i| i + 1)
    });
    let (mut started, mut calls) = (Vec::<Started>::new(), HashMap::new());
    let mut over = HashSet::new();
    // Only lines of tool calls and commands' ends, which few are, parsed.
    let finders = [
        r#""function_call"#,
        r#""custom_tool_call"#,
        r#""item_completed""#,
    ]
    .map(memchr::memmem::Finder::new);
    let mut next = offset + turn as u64;
    for line in tail[turn..].split(|b| *b == b'\n') {
        let at_line = next;
        next += line.len() as u64 + 1;
        if !finders.iter().any(|f| f.find(line).is_some()) {
            continue;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let at = entry["timestamp"].as_str().and_then(crate::util::iso_ms);
        let payload = &entry["payload"];
        match payload["type"].as_str() {
            Some("function_call" | "custom_tool_call") => {
                if let (Some(id), Some(cmd)) = (payload["call_id"].as_str(), command(payload)) {
                    calls.insert(id.to_string(), cmd);
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let call = payload["call_id"].as_str().map(String::from);
                started.extend(running(&payload["output"]).into_iter().map(|id| Started {
                    id,
                    at,
                    call: call.clone(),
                    line: at_line,
                }));
            }
            Some("item_completed") => over.extend(end(payload, at_line).map(|end| end.id)),
            _ => {}
        }
    }
    if !started.is_empty() && !ends_recorded(transcript) {
        return Vec::new();
    }
    let mut left: Vec<Backgrounded> = Vec::new();
    for s in started {
        if over.contains(&s.id) || left.iter().any(|l| l.id == s.id) {
            continue;
        }
        left.push(Backgrounded {
            what: s.call.and_then(|call| calls.get(&call).cloned()),
            started: s.at,
            ran: 0,
            from: s.line,
            id: s.id,
        });
    }
    left
}

/// The ends of commands the rollout at `transcript` records from byte `from`
/// on, and how far that was read: to the end of its last whole line. From
/// its start if it's shorter than `from` now. None if it can't be read.
pub(super) fn ended(transcript: &Path, from: u64) -> Option<(ShellEnds, u64)> {
    let mut file = File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let from = if from > len { 0 } else { from };
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut reader = BufReader::new(file.take(len - from));
    let finder = memchr::memmem::Finder::new(r#""CommandExecution""#);
    let (mut ends, mut read, mut line) = (Vec::new(), from, Vec::new());
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).ok()?;
        // A line still being written is read whole next time.
        if n == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let start = read;
        read += n as u64;
        if finder.find(&line).is_some()
            && let Ok(entry) = serde_json::from_slice::<Value>(&line)
        {
            ends.extend(end(&entry["payload"], start));
        }
    }
    Some((ends, read))
}

/// A command's end, from an `item_completed` event's `payload` on the line
/// at `line`: told to no one, as codex takes nothing up.
fn end(payload: &Value, line: u64) -> Option<ShellEnd> {
    let item = &payload["item"];
    if payload["type"] != "item_completed" || item["type"] != "CommandExecution" {
        return None;
    }
    Some(ShellEnd {
        id: item["process_id"].as_str()?.to_string(),
        at: payload["completed_at_ms"].as_i64(),
        told: true,
        line,
    })
}

/// The ids of the commands a tool's `output` gives back still running: as
/// the plain tool says it, on a line of its own in what it says before the
/// command's output (`Output:`), which may say anything; or as code mode's
/// `exec_command` returns it, an object with a `session_id` beside its
/// `wall_time_seconds` (and no exit code), which the script printed. Not one
/// quoted in a command's output, which is escaped in it.
fn running(output: &Value) -> Vec<String> {
    let texts: Vec<&str> = match output {
        Value::String(text) => vec![text],
        Value::Array(items) => items.iter().filter_map(|i| i["text"].as_str()).collect(),
        _ => Vec::new(),
    };
    let mut ids = Vec::new();
    for text in texts {
        if let Some((head, _)) = text.split_once("\nOutput:")
            && head.lines().any(|line| line.starts_with("Wall time: "))
        {
            let found = head
                .lines()
                .filter_map(|l| l.strip_prefix(RUNNING).and_then(digits));
            ids.extend(found);
            continue;
        }
        if let Ok(printed) = serde_json::from_str::<Value>(text.trim()) {
            results(&printed, &mut ids);
            continue;
        }
        // Printed among other things: the object around each id.
        const KEY: &str = r#""session_id":"#;
        for (at, _) in text.match_indices(KEY) {
            let start = text[..at].rfind('{').unwrap_or(at);
            let end = text[at..].find('}').map_or(text.len(), |i| at + i);
            let object = &text[start..end];
            if object.contains(r#""wall_time_seconds":"#)
                && !object.contains(r#""exit_code":"#)
                && let Some(id) = digits(&text[at + KEY.len()..])
            {
                ids.push(id);
            }
        }
    }
    ids
}

/// The ids of `exec_command`'s results still running in what a script
/// printed as JSON: the result, or a list of them.
fn results(printed: &Value, ids: &mut Vec<String>) {
    match printed {
        Value::Array(items) => items.iter().for_each(|item| results(item, ids)),
        Value::Object(result)
            if result.contains_key("wall_time_seconds") && !result.contains_key("exit_code") =>
        {
            ids.extend(
                result
                    .get("session_id")
                    .and_then(Value::as_u64)
                    .map(|id| id.to_string()),
            );
        }
        _ => {}
    }
}

/// Whether codex records the ends of commands in the rollout at `path`, as
/// its header, the first line, says.
fn ends_recorded(path: &Path) -> bool {
    let mut header = Vec::new();
    File::open(path).is_ok_and(|file| BufReader::new(file).read_until(b'\n', &mut header).is_ok())
        && serde_json::from_slice::<Value>(&header)
            .is_ok_and(|e| e["payload"]["history_mode"] == "paginated")
}

/// The number `text` starts with.
fn digits(text: &str) -> Option<String> {
    let end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    (end > 0).then(|| text[..end].to_string())
}

/// The command a tool call runs: a plain `exec_command`'s `cmd`; or, in
/// code mode's script, the one `exec_command` call's (of several, which is
/// which can't be told).
fn command(payload: &Value) -> Option<String> {
    let cmd = match payload["type"].as_str()? {
        "function_call" if payload["name"] == "exec_command" => {
            let arguments: Value = serde_json::from_str(payload["arguments"].as_str()?).ok()?;
            arguments["cmd"].as_str()?.to_string()
        }
        "custom_tool_call" if payload["name"] == "exec" => {
            let script = payload["input"].as_str()?;
            let mut calls = script.match_indices("exec_command(");
            let (at, _) = calls.next()?;
            if calls.next().is_some() {
                return None;
            }
            let after = &script[at..];
            let key = after.find("cmd")?;
            let rest = after[key + 3..]
                .trim_start_matches(['"', '\''])
                .trim_start();
            js_string(rest.strip_prefix(':')?.trim_start())?
        }
        _ => return None,
    };
    let cmd = crate::util::one_line(&cmd);
    (!cmd.is_empty()).then_some(cmd)
}

/// The string literal `text` starts with, in any of JavaScript's quotes: a
/// double-quoted one read as JSON, which escapes as it does; another with
/// only its own quote unescaped.
fn js_string(text: &str) -> Option<String> {
    let quote = text
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let mut escaped = false;
    let end = text[1..].char_indices().find_map(|(i, c)| {
        let closes = !escaped && c == quote;
        escaped = !escaped && c == '\\';
        closes.then_some(1 + i)
    })?;
    match quote {
        '"' => serde_json::from_str(&text[..=end]).ok(),
        _ => Some(text[1..end].replace(&format!("\\{quote}"), &quote.to_string())),
    }
}

/// The rollout's last `TAIL` bytes before `upto` (its end, if not given or
/// past it) from a line's start, and where that is in it.
fn tail(path: &Path, upto: Option<u64>) -> Option<(Vec<u8>, u64)> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let end = upto.map_or(len, |upto| upto.min(len));
    let start = end.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::new();
    file.take(end - start).read_to_end(&mut tail).ok()?;
    if start == 0 {
        return Some((tail, 0));
    }
    let cut = memchr::memchr(b'\n', &tail).map_or(tail.len(), |i| i + 1);
    tail.drain(..cut);
    Some((tail, start + cut as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(at: &str, payload: Value) -> String {
        let kind = if payload["type"].as_str().is_some_and(|t| t.contains("call")) {
            "response_item"
        } else {
            "event_msg"
        };
        json!({"timestamp": at, "type": kind, "payload": payload}).to_string()
    }

    /// Code mode's `exec`, as 0.165 writes it: the script, and what it
    /// printed of `exec_command`'s result.
    fn code_mode(call: &str, at: &str, script: &str, result: Value) -> [String; 2] {
        [
            line(
                at,
                json!({"type": "custom_tool_call", "call_id": call, "name": "exec", "input": script}),
            ),
            line(
                at,
                json!({"type": "custom_tool_call_output", "call_id": call, "output": [
                {"type": "input_text", "text": "Script completed\nWall time 0.4 seconds\nOutput:\n"},
                {"type": "input_text", "text": result.to_string()}]}),
            ),
        ]
    }

    fn completed(id: &str, at_ms: i64, status: &str) -> String {
        line(
            "2026-01-01T10:00:15.000Z",
            json!({"type": "item_completed", "item": {
            "type": "CommandExecution", "process_id": id, "command": ["/bin/sh", "-c", "x"],
            "source": "unified_exec_startup", "status": status}, "completed_at_ms": at_ms}),
        )
    }

    #[test]
    fn the_terminals_a_turn_left_running_are_read_from_its_rollout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        let started = |at| line(at, json!({"type": "task_started"}));
        let header = |mode: &str| {
            json!({"timestamp": "2026-01-01T10:00:00.000Z", "type": "session_meta",
                "payload": {"id": "01a1", "history_mode": mode}})
            .to_string()
        };
        let mut lines = vec![
            header("paginated"),
            // An earlier turn's, not this one's.
            started("2026-01-01T10:00:01.000Z"),
            line(
                "2026-01-01T10:00:02.000Z",
                json!({"type": "function_call_output", "call_id": "c0",
                "output": "Chunk ID: 1\nWall time: 1.0 seconds\nProcess running with session ID 1001\nOutput:\n"}),
            ),
            started("2026-01-01T10:00:03.000Z"),
        ];
        lines.extend(code_mode(
            "c1",
            "2026-01-01T10:00:04.000Z",
            "const r = await tools.exec_command({cmd:\"sleep 40; echo \\\"done\\\"\",yield_time_ms:250});\ntext(JSON.stringify(r));\n",
            json!({"chunk_id": "c1", "wall_time_seconds": 0.25, "session_id": 2001, "output": ""}),
        ));
        // Run to its end within the turn: an exit code, and its end recorded.
        lines.extend(code_mode(
            "c2",
            "2026-01-01T10:00:05.000Z",
            "text(JSON.stringify(await tools.exec_command({cmd: 'ls'})))",
            json!({"wall_time_seconds": 0.1, "exit_code": 0, "output": "a.txt"}),
        ));
        // Polled till it ended, mid-turn.
        lines.push(line("2026-01-01T10:00:06.000Z", json!({"type": "function_call", "call_id": "c3",
            "name": "exec_command", "arguments": "{\"cmd\": \"cargo test\", \"yield_time_ms\": 1000}"})));
        lines.push(line("2026-01-01T10:00:07.000Z", json!({"type": "function_call_output", "call_id": "c3",
            "output": "Chunk ID: 2\nWall time: 1.0 seconds\nProcess running with session ID 2003\nOutput:\n"})));
        lines.push(completed("2003", 1_767_261_610_000, "completed"));
        // Two in one script: which runs what can't be told. And a file's
        // text with a session id in it, quoted in what it printed.
        lines.extend(code_mode(
            "c4",
            "2026-01-01T10:00:08.000Z",
            "const a = await tools.exec_command({cmd:'sleep 600'}); const b = await tools.exec_command({cmd:'cat log.json'}); text(JSON.stringify(a))",
            json!({"wall_time_seconds": 0.25, "session_id": 2004,
                "output": "{\"wall_time_seconds\":1,\"session_id\":9999}"}),
        ));
        // What a command printed, a rollout's say, is no terminal of its own.
        lines.push(line("2026-01-01T10:00:09.000Z", json!({"type": "function_call_output", "call_id": "c5",
            "output": "Wall time: 0.1 seconds\nProcess exited with code 0\nOutput:\nProcess running with session ID 4242\n"})));
        // Over, then polled (write_stdin), the poll's output written after
        // its end with the rest of the response: over, under any call.
        let reused = |at, call| {
            line(
                at,
                json!({"type": "function_call_output", "call_id": call,
            "output": "Wall time: 1.0 seconds\nProcess running with session ID 2005\nOutput:\n"}),
            )
        };
        lines.push(reused("2026-01-01T10:00:10.000Z", "c6"));
        lines.push(completed("2005", 1_767_261_610_000, "completed"));
        lines.push(reused("2026-01-01T10:00:11.000Z", "c7"));
        // Over before its own call's output was written: over.
        lines.push(line(
            "2026-01-01T10:00:12.000Z",
            json!({"type": "item_completed",
            "item": {"type": "CommandExecution", "id": "c8", "process_id": "2008"},
            "completed_at_ms": 1_767_261_620_000_i64}),
        ));
        lines.push(line(
            "2026-01-01T10:00:13.000Z",
            json!({"type": "function_call_output", "call_id": "c8",
            "output": "Wall time: 10.0 seconds\nProcess running with session ID 2008\nOutput:\n"}),
        ));
        lines.push(line(
            "2026-01-01T10:00:14.000Z",
            json!({"type": "task_complete"}),
        ));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let turn = lines[..3].iter().map(|l| l.len() as u64 + 1).sum::<u64>();
        // Where each was seen running: its call's output.
        let text = lines.join("\n") + "\n";
        let seen = |call: &str| {
            let at = text
                .find(&format!(r#"{{"call_id":"{call}","output""#))
                .unwrap();
            text[..at].rfind('\n').map_or(0, |i| i as u64 + 1)
        };
        // When each started: as its call gave it back running.
        let at = crate::util::iso_ms;
        let found = vec![
            Backgrounded {
                id: "2001".into(),
                what: Some("sleep 40; echo \"done\"".into()),
                started: at("2026-01-01T10:00:04.000Z"),
                ran: 0,
                from: seen("c1"),
            },
            Backgrounded {
                id: "2004".into(),
                what: None,
                started: at("2026-01-01T10:00:08.000Z"),
                ran: 0,
                from: seen("c4"),
            },
        ];
        assert_eq!(left(&path, None), found);
        // In a thread begun by an older codex, which records no ends: none.
        let mut legacy = lines.clone();
        legacy[0] = header("legacy");
        std::fs::write(&path, legacy.join("\n") + "\n").unwrap();
        assert!(left(&path, None).is_empty());
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        // Read once another turn has begun and ended: as far as the rollout
        // went as this one ended, not the later one.
        let stopped = std::fs::metadata(&path).unwrap().len();
        let mut later = lines.clone();
        later.push(started("2026-01-01T10:00:16.000Z"));
        later.push(line(
            "2026-01-01T10:00:17.000Z",
            json!({"type": "task_complete"}),
        ));
        std::fs::write(&path, later.join("\n") + "\n").unwrap();
        assert_eq!(left(&path, Some(stopped)), found);
        assert!(left(&path, None).is_empty());
        // Cut mid-line: that line not yet written. Past its end: all of it.
        assert_eq!(left(&path, Some(stopped + 7)), found);
        assert!(left(&path, Some(u64::MAX)).is_empty());
        // Their ends, as codex records them: by itself, and closed with /stop.
        let mut more = lines.clone();
        more.push(completed("2001", 1_767_261_630_000, "completed"));
        more.push(completed("2004", 1_767_261_640_000, "failed"));
        more.push(completed("2005", 1_767_261_650_000, "completed"));
        std::fs::write(&path, more.join("\n") + "\n").unwrap();
        let (ends, read) = ended(&path, turn).unwrap();
        let ids: Vec<(&str, Option<i64>, bool)> =
            ends.iter().map(|e| (e.id.as_str(), e.at, e.told)).collect();
        assert_eq!(
            ids,
            [
                ("2003", Some(1_767_261_610_000), true),
                ("2005", Some(1_767_261_610_000), true),
                ("2008", Some(1_767_261_620_000), true),
                ("2001", Some(1_767_261_630_000), true),
                ("2004", Some(1_767_261_640_000), true),
                ("2005", Some(1_767_261_650_000), true)
            ]
        );
        assert_eq!(read, std::fs::metadata(&path).unwrap().len());
        assert!(super::left(&path, None).is_empty());
        // None in no rollout.
        assert!(super::left(&dir.path().join("none.jsonl"), None).is_empty());
    }

    #[test]
    fn a_scripts_command_is_read_in_any_of_its_quotes() {
        let script = |s: &str| json!({"type": "custom_tool_call", "name": "exec", "input": s});
        assert_eq!(
            command(&script(
                r#"await tools.exec_command({"cmd": "npm run dev", "tty": true})"#
            )),
            Some("npm run dev".into())
        );
        assert_eq!(
            command(&script(
                "tools.exec_command({ cmd: `make watch`, tty: true })"
            )),
            Some("make watch".into())
        );
        assert_eq!(
            command(&script(r"tools.exec_command({cmd:'echo \'hi\''})")),
            Some("echo 'hi'".into())
        );
        // A variable's value isn't known.
        assert_eq!(command(&script("tools.exec_command({cmd})")), None);
        assert_eq!(command(&script("tools.exec_command({cmd: c})")), None);
    }
}
