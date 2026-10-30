//! The commands Claude leaves running in the background, its shells: from
//! its hooks as one is left running and as it's told one ended, and from its
//! transcript for an end it's told mid-turn, which no hook reports.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::agent::{Backgrounded, Report, ShellEnd, ShellEnds};

/// How Claude's notice that something it left running ended begins.
const NOTICE: &str = "<task-notification>";

/// What a hook's `payload` says of Claude's shells, into `report`: the
/// command a tool call just left running in the background, and those it
/// says have ended. Not a subagent's, whose shells end with its final
/// reply, nor one Claude ends with its own turn.
pub(super) fn read(payload: &Value, report: &mut Report) {
    // Told one ended as a prompt of its own: a turn it starts by itself.
    if let Some(prompt) = payload.get("prompt").and_then(Value::as_str)
        && prompt.trim_start().starts_with(NOTICE)
    {
        report.woken = true;
        report.shells_ended = task_ids(prompt);
        return;
    }
    if payload.get("hook_event_name").and_then(Value::as_str) != Some("PostToolUse") {
        return;
    }
    let input = payload.get("tool_input");
    match payload.get("tool_name").and_then(Value::as_str) {
        Some("Bash") if report.subagent.is_none() => {
            let response = payload.get("tool_response");
            let field = |name| response.and_then(|r| r.get(name));
            let with_turn = field("backgroundEndsWithFinalResponse").and_then(Value::as_bool);
            if let Some(id) = field("backgroundTaskId").and_then(Value::as_str)
                && with_turn != Some(true)
            {
                let what = (input.and_then(|i| i.get("description")))
                    .and_then(Value::as_str)
                    .map(crate::agent::clean)
                    .filter(|d| !d.is_empty());
                report.shells.push(Backgrounded {
                    id: id.to_string(),
                    what,
                    started: None,
                    // Put there by you or a timeout, it ran a while first.
                    ran: (payload.get("duration_ms").and_then(Value::as_i64)).unwrap_or(0),
                    from: (payload.get(crate::report::TRANSCRIPT_SIZE))
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                });
            }
        }
        // Stopped by Claude itself (`KillShell`, its `shell_id`, in earlier versions).
        Some("TaskStop" | "KillShell") => {
            let id = input.and_then(|i| i.get("task_id").or_else(|| i.get("shell_id")));
            report
                .shells_ended
                .extend(id.and_then(Value::as_str).map(String::from));
        }
        _ => {}
    }
}

/// What Claude left running whose end its transcript at `transcript`
/// records from byte `from` on, and how far that was read: to the end of
/// its last whole line. From its start if it's shorter than `from` now
/// (written anew). None if it can't be read.
pub(super) fn ended(transcript: &Path, from: u64) -> Option<(ShellEnds, u64)> {
    let mut file = File::open(transcript).ok()?;
    let len = file.metadata().ok()?.len();
    let from = if from > len { 0 } else { from };
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut reader = BufReader::new(file.take(len - from));
    let finder = memchr::memmem::Finder::new(NOTICE);
    let (mut ids, mut read, mut line) = (Vec::new(), from, Vec::new());
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
            && let Some((notice, at, told)) = notice_in(&line)
        {
            let end = |id| ShellEnd {
                id,
                at,
                told,
                line: start,
            };
            ids.extend(task_ids(&notice).into_iter().map(end));
        }
    }
    Some((ids, read))
}

/// The notice a transcript's `line` gives Claude, as it's queued, as it
/// reaches Claude mid-turn, or as the prompt of a turn of its own; with when
/// it was written, and whether Claude was told it mid-turn, which nothing
/// else reports (a turn of its own reports its prompt itself); not one quoted
/// in what a tool read or wrote, nor in a prompt of yours.
fn notice_in(line: &[u8]) -> Option<(String, Option<i64>, bool)> {
    let entry: Value = serde_json::from_slice(line).ok()?;
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(String::from);
    let (notice, told) = match entry.get("type").and_then(Value::as_str)? {
        "queue-operation" if entry.get("operation")?.as_str()? == "enqueue" => {
            (text(entry.get("content")), false)
        }
        "attachment" => {
            let attachment = entry.get("attachment")?;
            let queued = attachment.get("type")?.as_str()? == "queued_command";
            (queued.then(|| text(attachment.get("prompt")))?, true)
        }
        "user" if entry.pointer("/origin/kind")?.as_str()? == "task-notification" => {
            (text(entry.pointer("/message/content")), false)
        }
        _ => return None,
    };
    let notice = notice.filter(|notice| notice.trim_start().starts_with(NOTICE))?;
    let at = (entry.get("timestamp").and_then(Value::as_str)).and_then(crate::util::iso_ms);
    Some((notice, at, told))
}

/// The id of the task each notice in `text` is about: its own, not one
/// quoted in what the task left (an agent's result). Claude puts its
/// notices' tags at the start or the end of a line, so one a text only
/// mentions doesn't count.
fn task_ids(text: &str) -> Vec<String> {
    const END: &str = "</task-notification>";
    let (mut ids, mut depth, mut found) = (Vec::new(), 0usize, false);
    for line in text.lines().map(str::trim_start) {
        let mut rest = line;
        if let Some(after) = line.strip_prefix(NOTICE) {
            depth += 1;
            if depth == 1 {
                found = false;
            }
            rest = after;
        }
        if depth == 1
            && !found
            && let Some(start) = rest.find("<task-id>")
            && rest[..start].trim().is_empty()
            && let Some((id, _)) = rest[start + "<task-id>".len()..].split_once("</task-id>")
        {
            ids.push(id.trim().to_string());
            found = true;
        }
        if line.starts_with(END) || (line != rest && rest.trim_end().ends_with(END)) {
            depth = depth.saturating_sub(1);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_payload(payload: Value, subagent: Option<&str>) -> Report {
        let mut report = Report {
            subagent: subagent.map(String::from),
            ..Report::default()
        };
        read(&payload, &mut report);
        report
    }

    fn notice(id: &str, status: &str) -> String {
        format!(
            "<task-notification>\n<task-id>{id}</task-id>\n<status>{status}</status>\n</task-notification>"
        )
    }

    #[test]
    fn a_command_left_running_is_reported_as_its_tool_call_returns() {
        let left = json!({
            "hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": "npm test", "description": " Run the\ttests\u{202e} ", "run_in_background": true},
            "tool_response": {"stdout": "", "backgroundTaskId": "b1"},
            "duration_ms": 4000,
            crate::report::TRANSCRIPT_SIZE: 1234,
        });
        let report = read_payload(left.clone(), None);
        // Nothing a terminal takes for more than text; how long it ran
        // before it was left running (put there by Ctrl+B, say).
        let shell = Backgrounded {
            id: "b1".into(),
            what: Some("Run the tests".into()),
            started: None,
            ran: 4000,
            from: 1234,
        };
        assert_eq!(report.shells, [shell]);
        assert!(!report.woken);
        // A subagent's ends with its final reply.
        assert_eq!(read_payload(left.clone(), Some("a1")).shells, []);
        // As does one Claude ends with its own turn.
        let mut with_turn = left;
        with_turn["tool_response"]["backgroundEndsWithFinalResponse"] = json!(true);
        assert_eq!(read_payload(with_turn, None).shells, []);
        // A command that finished in the foreground left nothing running.
        let done = json!({"hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": {"command": "ls"}, "tool_response": {"stdout": "a"}});
        assert_eq!(read_payload(done, None).shells, []);
    }

    #[test]
    fn an_end_is_told_as_a_prompt_or_by_stopping_it() {
        let prompt = |text: String| {
            read_payload(
                json!({"hook_event_name": "UserPromptSubmit", "prompt": text}),
                None,
            )
        };
        let woken = prompt(format!(
            "{}\n{}",
            notice("b1", "completed"),
            notice("b2", "killed")
        ));
        assert!(woken.woken);
        assert_eq!(woken.shells_ended, ["b1", "b2"]);
        // A notice whose result quotes another's is about its own task only.
        let result = format!(
            "completed</status>\n<result>{}</result>",
            notice("b3", "completed")
        );
        assert_eq!(prompt(notice("a1", &result)).shells_ended, ["a1"]);
        // Its tags only mentioned (a command about them) don't count.
        let mention = "completed</status>\n<summary>Grep for <task-notification> and </task-notification> tags</summary>";
        let both = format!("{}\n{}", notice("b4", mention), notice("b5", "completed"));
        assert_eq!(prompt(both).shells_ended, ["b4", "b5"]);
        // One on a line of its own.
        let one_line = "<task-notification><task-id>b6</task-id></task-notification>\n<task-notification><task-id>b7</task-id></task-notification>";
        assert_eq!(prompt(one_line.into()).shells_ended, ["b6", "b7"]);
        let yours = prompt("fix it".into());
        assert!(!yours.woken && yours.shells_ended.is_empty());

        let stop = |event: &str, tool: &str, input: Value| {
            let payload = json!({"hook_event_name": event, "tool_name": tool, "tool_input": input});
            read_payload(payload, None).shells_ended
        };
        assert_eq!(
            stop("PostToolUse", "TaskStop", json!({"task_id": "b1"})),
            ["b1"]
        );
        assert_eq!(
            stop("PostToolUse", "KillShell", json!({"shell_id": "b2"})),
            ["b2"]
        );
        // Asked about, not done yet.
        assert!(stop("PermissionRequest", "TaskStop", json!({"task_id": "b1"})).is_empty());
    }

    #[test]
    fn an_end_told_mid_turn_is_read_from_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.jsonl");
        let lines = [
            json!({"type": "user", "message": {"role": "user", "content": "go"}}),
            // A tool's output that quotes a notice: not one.
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": notice("b0", "completed")}]}}),
            json!({"type": "queue-operation", "operation": "enqueue", "content": notice("b1", "completed"),
                "timestamp": "2026-01-01T10:00:00.500Z"}),
            json!({"type": "queue-operation", "operation": "remove", "content": notice("b9", "completed")}),
            // A prompt of yours queued mid-turn that quotes one: not one either.
            json!({"type": "attachment", "attachment": {"type": "queued_command",
                "prompt": format!("why did this fail? {}", notice("b8", "failed"))}}),
            json!({"type": "attachment", "attachment": {"type": "queued_command", "prompt": notice("b2", "failed")}}),
            json!({"type": "user", "origin": {"kind": "task-notification"}, "message": {"content": notice("b3", "stopped")}}),
        ];
        let mut text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let whole = text.len() as u64;
        // Still being written.
        text.push_str(&format!(
            r#"{{"type":"queue-operation","operation":"enqueue","content":"{}"#,
            "<task-notification>"
        ));
        std::fs::write(&path, &text).unwrap();

        let ids =
            |found: ShellEnds| -> Vec<String> { found.into_iter().map(|end| end.id).collect() };
        let (found, read) = ended(&path, 0).unwrap();
        // Each with when it was written, where that's said.
        let at = crate::util::iso_ms("2026-01-01T10:00:00.500Z");
        // Whether Claude was told it mid-turn, not only had it queued or
        // given as a prompt (which a turn of its own reports).
        // And where its line starts.
        let start = |n: usize| (lines[..n].iter().map(|l| l.to_string().len() as u64 + 1)).sum();
        let end = |id: &str, at, told, n| ShellEnd {
            id: id.into(),
            at,
            told,
            line: start(n),
        };
        assert_eq!(found[0], end("b1", at, false, 2));
        assert_eq!(found[1], end("b2", None, true, 5));
        assert_eq!(found[2], end("b3", None, false, 6));
        assert_eq!(ids(found), ["b1", "b2", "b3"]);
        assert_eq!(read, whole);
        // From where it read up to: nothing new.
        assert_eq!(ended(&path, read).unwrap(), (Vec::new(), whole));
        // From the middle of a line, that line isn't one.
        let third = lines[..3]
            .iter()
            .map(|l| l.to_string().len() as u64 + 1)
            .sum::<u64>();
        assert_eq!(ids(ended(&path, third - 5).unwrap().0), ["b2", "b3"]);
        // Written anew, shorter: from its start.
        assert_eq!(ids(ended(&path, whole * 2).unwrap().0), ["b1", "b2", "b3"]);
        assert_eq!(ended(&dir.path().join("none.jsonl"), 0), None);
    }
}
